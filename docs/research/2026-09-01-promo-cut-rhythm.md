<!-- Generated 2026-09-01 via /research (murmur-researcher fan-out, 2 angles). Pricing/funding/version = point-in-time. -->
# Research: promo cut rhythm — action → description → transition → action

## TL;DR / Verdict

**The camera drift added on 2026-09-01 is an anti-pattern, and it was a symptom-fix for the wrong
disease.** Two independent angles converge on the same three conclusions:

1. **Nobody drifts.** Phase-correlation global-motion measurement of 13 product videos from 10
   named companies (Linear, Notion, Raycast, Arc, Superhuman, Descript, Tella, Screen Studio,
   Apple, Granola) finds the reference grammar is strictly **bimodal**: dead-locked (&lt;1 px/s) or
   moving decisively (&gt;15 px/s). Every reference film spends **0–28%** of its runtime in the
   "visible but purposeless" 1–15 px/s band. Our cut spends **64%** there. `driftAt` computed
   analytically from its own source is in that band **86.4%** of the time and locked **0.3%**.
2. **The dead time is a symptom of unreadable captions, not an independent defect.** All 15 caption
   blocks are below every broadcast reading standard — a **~56 s collective deficit** — so the edit
   is forced to hold a frozen picture while the viewer reads text they cannot finish anyway.
3. **The motion metric was Goodharted.** `README.md`'s frame-difference gate measures the RENDER, so
   a global translation raises it everywhere at once. A 16 px sinusoid was the cheapest possible way
   to move "50% frozen" to "11%" without adding one bit of information.

**The real defect is that the footage does not perform.** `capture/` — the hook — is **99% unchanged
frames**; the note body is fully written at frame 0; the scene captioned *"Type while it listens"*
**never types anything**. `film.type()` exists with a real 26 cps delay and is used **once** in
92.8 s. The film has **zero hard cuts**.

Fix belongs in `record.mjs` (perform more), not `compose.html` (move the camera).

## What we already have (from repo)

- `scripts/promo/compose.html:723` — `driftAt()`, the rejected drift; `:754` `breathAt()`, a 3%
  per-scene push; `:1129` the aurora parallax that slides the ground with it.
- `scripts/promo/README.md:96-112` — "The motion floor" and the frame-difference gate this brief
  argues is measuring the wrong artefact.
- `scripts/promo/record.mjs:213-216` — `film.type(target, text, {cps:26})`, used once (`:509`).
- `scripts/promo/record.mjs:367` — `SCENES`, 9 scenes / 92.8 s, **0 hard cuts** (all dissolve,
  push, whip, through).
- `docs/research/2026-08-31-app-promo-video.md:62-63` — **our own prior rule, broken by the drift:**
  *"Motion with intent. Every zoom, pan and cut points at something. No drifting 'for production
  value'."*

**Measured here (2026-09-01), on the shipped cut:**

| measurement | value |
| --- | --- |
| time the APP is doing nothing (source-frame gaps &gt;400 ms) | **48.5 s of 93.6 s = 52%** |
| camera moves vs diegetic events (`record.mjs`) | **34 vs 16** — one app event every 5.8 s |
| captions below reading threshold | **15 / 15**, deficit **56 s** |
| global-motion band, 1–15 px/s ("mush") | **64%** (reference films: 0–28%) |
| locked, &lt;1 px/s | **16%** (Linear 73%, Superhuman 100%, Arc loops 100%) |

## Findings

### 1. Prior art: the reference grammar is bimodal (confidence: high — direct measurement)

| Video | Dur | LOCKED &lt;1 px/s | MUSH 1–15 | MOVING &gt;15 | median px/s |
| --- | --- | --- | --- | --- | --- |
| Linear — *Intro to Linear* | 240.6 s | **73%** | 4% | 23% | **0.01** |
| Notion — *What is Notion?* | 86.9 s | 54% | 12% | 34% | 0.54 |
| Raycast — *New Raycast 2026* | 38.5 s | 41% | 15% | 44% | 2.64 |
| Arc — *Arc 1.0* | 34.3 s | 49% | 3% | 48% | 1.79 |
| Notion homepage hero | 11.0 s | 88% | 12% | 0% | 0.21 |
| Superhuman hero | 7.0 s | **100%** | 0% | 0% | **0.00** |
| Arc feature loops (×3) | 3.0 s | **100%** | 0% | 0% | ~0.00 |
| Descript hero | 25.5 s | 77% | 13% | 10% | — |
| Tella hero | 31.2 s | 90% | 3% | 7% | — |
| Screen Studio hero | 6.3 s | 22% | 24% | 54% | 25.42 |
| **Murmur — current cut** | 92.8 s | **16%** | **64%** | 20% | 4.51 |

Linear holds a single locked frame for **17.5 s** straight. Superhuman's hero is *exactly* 0.00 px/s
for 7 s. When Raycast and Arc move, their p75 is 93 and 137 px/s — decisive. Our p75 is 10 px/s: we
are almost never still and almost never decisive.

`linear.app`, `vercel.com`, `raycast.com`, `stripe.com`, `granola.ai` and `apple.com/mac` ship
homepages with **zero `<video>` elements** — for them the motion *is* the animated interface.

### 2. Where motion comes from, in order of use (confidence: high)

1. **The interface itself.** Notion's 11 s hero: the window is pixel-identical in position across
   six sampled frames; what changes is content — cards appear, avatars pop, rows fill. It holds 9.5 s
   locked and never reads as a screenshot.
2. **The cut.** The cheapest "something happened", at zero motion cost. Raycast cuts 7× in 38.5 s;
   Arc 20× in 34.3 s.
3. **Motivated camera, only with a destination.** Screen Studio's hero is the archetype: p10 =
   0.3 px/s with a median of 25 px/s — rest → decisive push into the clicked cell → rest.

### 3. "Don't move while they read" is documented three times over (confidence: high)

- **Apple HIG, Motion:** *"Don't add motion for the sake of adding motion."* And, specifically:
  *"Avoid showing objects that oscillate in a sustained way. In particular, you want to avoid showing
  an oscillation that has a frequency of around 0.2 Hz because people can be very sensitive to this
  frequency."* **Our four drift terms are 0.110, 0.257, 0.093 and 0.224 Hz** — two of them land
  essentially on the named worst case, and all four sit inside the ISO 2631-1 motion-sickness band
  (0.1–0.5 Hz, weighting peak ~0.17–0.23 Hz).
- **WCAG 2.2 SC 2.2.2 Pause, Stop, Hide:** moving content *"can be a barrier to anyone who has
  trouble reading stationary text quickly."* Our drift is auto-starting motion lasting 92.8 s in
  parallel with text the viewer must read.
- **`prefers-reduced-motion` (MDN):** *"scaling or panning large objects can be vestibular motion
  triggers."* We pan the largest object in frame continuously — and because it is baked into the
  MP4, the media query cannot honour it. `angular-zoneless.md` §6 binds the *app* to reduced motion;
  the marketing asset currently cannot comply.
- Reading research: animated backgrounds significantly slow reading (Vital et al., HCII 2022), but
  the dominant lever is the **backing panel**, not the stillness of what is behind it (Jankowski et
  al., CHI '10 — billboard style produced the best performance). **Keep `#scrim`.**

### 4. Ken Burns does not license this (confidence: high)

The technique is defined as pan/zoom on **still photographs**, *"principally used when film or video
material is not available"*. Our footage is a live application. Applying a still-photo rescue to a
screen recording is treating the product as a screenshot — the exact failure the drift was meant to
avoid. The rationale inverts into an argument against it.

### 5. Cutting faster is NOT the answer either (confidence: medium-high)

- Beat length **8–10 s is correct**: Mayer's segmenting principle, 10/10 tests, median d = 0.79 —
  *"each verbal segment be short — involving 8–10 seconds of speech concerning one main event."*
  Our 9.3 s/beat is squarely inside it.
- MacLachlan &amp; Logan (*J. Advertising Research* 33(2), 1993): past **12 shots in a 30 s spot,
  persuasion declines**; 1–5-shot commercials scored 36% higher than 20+. *(Directional: 1993
  broadcast-TV data, primary paywalled.)*
- Redfern (2022) cautions that running-time ÷ shot-count is a crude proxy, not a style measure.

**The beat and the shot are different units.** The beat is right; the *event cadence inside it* is
wrong.

### 6. Caption reading budget (confidence: high — arithmetic)

Netflix Timed Text Style Guide (**20 cps**, 42 chars/line, **≤2 lines**, min 5/6 s, **max 7 s per
event**) and BBC practice (**160–180 wpm**) converge on **≈0.33 s per word**, counting *all*
on-screen text — kicker, headline and sub together.

> **`dur` ≈ 0.33 × words + 0.6 s** (the 0.6 s covers the per-word stagger settle:
> `IN_MS 460` + `STAGGER 38` at `compose.html:854-856`)

| words | `dur` to author |
| --- | --- |
| 4 | 1.9 s |
| 6 | 2.6 s |
| 8 | 3.3 s |
| 12 | 4.6 s |
| 20 | 7.3 s — **over Netflix's 7 s ceiling; split into two events** |

Hard ceiling **≤84 characters on screen**. Our blocks run 82–144 (median 122). Headlines alone pass
13/15; it is the 14 `sub:` lines (median 77 chars) that break every block *and* create the holds.

## Fit with Murmur's constraints

Marketing-asset edit under `scripts/promo/`: no Rust, no SQLite, no provider seam, no lock model, no
egress path. Three constraints bind:

- **Privacy gate stays.** Adding performance to `record.mjs` means more UI states on camera; every
  one re-enters `film.assertClean()`. A published video cannot be un-published.
- **Honesty of footage.** *"Type while it listens"* over a note that never types is a lie by framing
  of exactly the class `README.md:160-163` records. Performing the scene fixes the caption *and* the
  motion in one move.
- **Accessibility consistency.** We honour `prefers-reduced-motion` in the app; shipping a hero that
  pans a large object at 0.22 Hz contradicts our own product ethic. This is positioning, not taste.

No new dependencies: `record.mjs` + `compose.html` edits plus the existing ffmpeg.

## Options and tradeoffs

**A — Delete the drift only (S, ~1 h). DO NOT SHIP ALONE.** Removes the complaint immediately, but
the raw footage is genuinely static, so the film reverts to a slideshow of screenshots and the
original problem returns worse.

**B — Delete the drift + PERFORM the scenes (M, ~1 day).** Type the note live in the hook; let lists
populate; click into the linked note; switch board tabs; expand the tree; open the locked workspace
*after* an unlocked one so the refusal has contrast. Motion becomes content and can never read as
"the app slides around." Risk: re-record (~3 min) + re-render (~19 min); scenes need re-timing.

**C — B + a real cut rhythm and re-timed captions (M/L, ~2 days).** Introduce genuine hard cuts
(we have zero), split 9 scenes into ~16–22 shots, re-time every caption to its own read speed and
cap copy at ~84 chars. Delivers the exact `action → description → transition → action` rhythm.

## Recommendation and first step

**Take B now, C next — and fix the metric first, because the metric is what produced the drift.**

Smallest verifiable slice:

1. **Re-point the motion metric at `.promo/frames/`** (the raw captured app footage), not
   `.promo/render/`. That measures *"is the app doing something"* and is immune to the camera
   exploit. Record today's RED baseline: `capture` 99% still, `brain` 99%, `people` 98%, `lock` 97%.
   Target: **no scene above 70% still frames**.
2. **Zero `driftAt` and the aurora parallax.** Keep `breathAt` but drop 3% → ~1.5%: it is
   unidirectional and beat-anchored, i.e. a push, not a wander.
3. **Perform the hook** — use `film.type()` so the note is written on camera under the caption that
   promises exactly that.
4. **Re-render, re-measure.** Acceptance: **MUSH ≤20%, LOCKED ≥45%** (currently 64% / 16%).

Shot-length brackets for the 90 s narrative cut:

| Beat type | Hold | Camera |
| --- | --- | --- |
| Opening 2 beats | 1.5–2.5 s | locked; cut, don't dissolve |
| Action beat (UI performing, no text) | 1.5–3 s | locked; cut on completion |
| Caption beat | max(3.0 s, chars ÷ 20 + 0.8 s), copy ≤84 chars | **locked for the whole hold** |
| Demo hold (multi-step action worth watching) | 6–10 s | locked; motion from the UI only |
| Motivated push (reveal / follow a click) | 0.6–1.2 s travel, then full stop | eased, ≥40 px/s peak |

Target **ASL 4–6 s → 16–22 shots**, with **≥6 genuine hard cuts**.

## Open questions / what I could not verify

- Corpus is purposive (n = 13, 10 companies), not random. Falsification was attempted against
  Descript and Tella and failed — both 77% / 90% locked. No named software product video was found
  using continuous unmotivated drift, but absence over 13 samples is not proof.
- `scdet` is threshold-sensitive and undercounts dissolve/morph films; Notion registers 3/4/14 cuts
  at thresholds 7/5/3.
- Vital et al. (2022) and Jankowski et al. (2010) are paywalled — direction confirmed via
  corroborating summaries and S2 metadata; effect sizes not read.
- BBC Subtitle Guidelines primary unreachable (proxy-blocked, and the GitHub mirror 404s); figures
  via Clevercast, which attributes them to the BBC. Netflix's numbers *are* primary and agree.
- MacLachlan &amp; Logan figures are secondary (JAR paywalled) and are 1993 broadcast-TV data.
- ISO 2631-1 weighting peak is search-derived; the standard is paywalled.
- The frame-difference method is a luminance proxy: it cannot distinguish "the app did something"
  from "the whole frame slid 0.2 px" — which is exactly how the drift passed the original gate.
  **A final verdict on whether a re-cut reads well needs a human watching it.**

## Sources

**Measured assets (fetched 2026-09-01):** Linear `youtube.com/watch?v=9Q5BoiIFBiY` · Notion
`watch?v=3HLMH9t1Q-g` · Raycast `watch?v=Mi173xGb0ZA` · Arc `watch?v=n5Vwrj1gEWs` · Notion hero
`videos.ctfassets.net/spoqsaf9291f/…hero-1920x1200_final.mp4` · Superhuman
`superhumanstatic.com/…hero-mail-desktop-2x.mp4` · Arc loops `arc.net/{theme-picker,space-swiping,zero-chrome}.mp4`
· Descript `static-cdn.descript.com/descript-website/videos/home-hero-transparent.webm` · Tella
`ucarecdn.com/761eb60c-f363-4a39-951a-8957732af1f1/` · Screen Studio
`screen.studio/videos/hero/hero-demo.mp4` · Apple `apple.com/105/media/us/os/shared/2026/…/anim/hero/large.mp4`
· zero-`<video>` homepages: `linear.app`, `vercel.com`, `raycast.com`, `stripe.com`, `granola.ai`,
`apple.com/mac`

**Guidance:** `developer.apple.com/design/human-interface-guidelines/motion` ·
`w3.org/WAI/WCAG22/Understanding/pause-stop-hide.html` ·
`developer.mozilla.org/en-US/docs/Web/CSS/@media/prefers-reduced-motion` ·
`partnerhelp.netflixstudios.com/hc/en-us/articles/217350977-English-Timed-Text-Style-Guide` ·
`partnerhelp.netflixstudios.com/hc/en-us/articles/360051554394-Timed-Text-Style-Guide-Subtitle-Timing-Guidelines`
· `clevercast.com/bbc-subtitling-guidelines/` · `cutscore.io/blog/how-fast-should-i-cut-my-video` ·
`en.wikipedia.org/wiki/Ken_Burns_effect`

**Research:** Mayer, *Multimedia Learning* ch. 8 (temporal contiguity, 8/8, d = 1.31) · Mayer &amp;
Pilegard, Cambridge Handbook ch. 13 (segmenting 10/10, d = 0.79, 8–10 s; modality 53/61, d = 0.76) ·
Vital, van der Baan, Stenberg &amp; Sandnes, HCII 2022, LNCS 13308 · Jankowski et al., CHI '10,
1321–1330 · MacLachlan &amp; Logan, *J. Advertising Research* 33(2), 1993 · Redfern, *Wide Screen*
9(1), 2022 · *Journal of Vision*, foveal displacement thresholds 0.68/0.96 arcmin at 0.08/0.24 °/s

**Code:** `scripts/promo/compose.html:723,754,821-822,1095,1129` ·
`scripts/promo/README.md:96-112` · `scripts/promo/record.mjs:213-216,367,374-428,509` ·
`docs/research/2026-08-31-app-promo-video.md:62-63`
