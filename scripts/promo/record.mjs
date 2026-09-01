/*
 * Murmur promo-video recorder — STAGE 1 of scripts/promo (see ./README.md).
 *
 * Drives the REAL shipping Angular UI over the screenshot harness's mocked Tauri
 * IPC + privacy-safe demo world (../screenshots/mock-tauri.js), and captures raw
 * footage as a numbered JPEG sequence plus a timeline of *authored* camera,
 * layout, caption and spotlight marks.
 *
 * Why frames-and-timestamps instead of Playwright's built-in recordVideo: that
 * API gives a viewport-sized VP8 webm at a best-effort frame rate, with no
 * pointer and no quality control — fine for a test artifact, not for something
 * that gets published. CDP's screencast hands us one frame per browser paint
 * with a presentation timestamp, so stage 2 can place every frame at the instant
 * it actually happened rather than assuming a constant rate.
 *
 * The capture runs at deviceScaleFactor 2 and the film is delivered at 1×, which
 * is what makes the camera work: a 2× push-in is native pixels, not an upscale.
 *
 * Usage (../screenshots/run.sh-style resolution of Playwright):
 *   PLAYWRIGHT_PATH=<npx-cache>/node_modules/playwright \
 *   MURMUR_URL=http://127.0.0.1:4310 node scripts/promo/record.mjs [scene...]
 */
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PATH || "playwright");

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "..", "..");
const SHOTS_DIR = join(ROOT, "scripts", "screenshots");
const MOCK = readFileSync(join(SHOTS_DIR, "mock-tauri.js"), "utf8");
const CURSOR = readFileSync(join(__dirname, "cursor.js"), "utf8");
const VERSION = JSON.parse(readFileSync(join(ROOT, "package.json"), "utf8")).version;

const BASE = process.env.MURMUR_URL || "http://localhost:1420";
const OUT = process.env.PROMO_DIR || join(ROOT, ".promo");

const EVENT_STATUS = "meetnotes://status";
const EVENT_LIVE_CAPTION = "murmur://live-caption";

/*
 * 16:9 at 1600×900 CSS px. The delivered film is 1920×1080 and the window inside
 * it is a KEYFRAMED object (see compose.html `LAYOUTS`), so one CSS pixel of UI
 * is between ~0.76 and ~1.04 delivered pixels depending on the shot — while the
 * captured pixels behind it are 2×, which is the headroom every push-in spends.
 */
const V = { width: 1600, height: 900 };

/*
 * The supersampling factor, and the ONE reason the push-ins are sharp.
 *
 * MEASURED, because it is not what you would assume: a context's
 * `deviceScaleFactor: 2` gives `page.screenshot()` a 2× PNG but leaves CDP
 * screencast frames at 1× (1600×900), and an explicit
 * `Emulation.setDeviceMetricsOverride` with deviceScaleFactor 2 does not change
 * that either — both were tried. What actually raises the compositor surface is
 * the LAUNCH flag `--force-device-scale-factor=2` (see `launchArgs` below).
 *
 * It matters because a 1× recording makes every camera move an upscale: at the
 * film's geometry a 1.8× push-in would resample ~1.7×, which is soft exactly
 * where the viewer is being asked to read something. `assertSupersampled` fails
 * the run if this ever silently regresses.
 */
const SCALE = 2;
const launchArgs = [`--force-device-scale-factor=${SCALE}`, "--high-dpi-support=1"];

/*
 * MEASURED, and the reason this runs in a visible window by default.
 *
 * Playwright's headless shell renders through SwiftShader — software raster —
 * and at 2× that is the bottleneck for the whole pipeline. The screencast only
 * emits a frame when the page paints one, so a slow renderer does not just cost
 * time, it costs FOOTAGE: typing and panel animations arrive as fewer, chunkier
 * samples. On the /ask scene (which types a question) the same take captured
 *
 *     headless (SwiftShader) …  7 fps
 *     headed   (real GPU)   … 19 fps
 *
 * and on /record, 19 fps against 99 fps. JPEG quality was ruled out first: 45
 * and 65 produced identical frame counts, so the cost is raster, not encode.
 *
 * A window will briefly appear. Set PROMO_HEADLESS=1 on a machine with no
 * display (the footage is still correct — just sampled more coarsely).
 */
const HEADLESS = !!process.env.PROMO_HEADLESS;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/*
 * ── The privacy gate, inherited ────────────────────────────────────────────
 * Identical in intent to scripts/screenshots/capture.mjs: a published marketing
 * asset cannot be un-published, so "no real data" has to be a property of the
 * tool. A video is worse than a screenshot here — a leak can sit in three frames
 * out of three thousand and nobody eyeballing the result will catch it — so we
 * scan the DOM at every authored beat, not just at the end.
 */
const PRIVACY_DENY = [
  [/jakub|gawronski|unitedrepaircentre|kgm004a/i, "operator identity"],
  [/\/Users\/(?!demo\b)[a-z]/i, "a real home directory"],
  [/weronika|leszek|alcon|organizator|mówca/i, "operator vault content (leaked into e2e fixtures)"],
  [/MeetNotes/, "the pre-rename product name"],
];
const EMAIL_RE = /[\w.+-]+@[\w.-]+/g;
const EMAIL_ALLOW = /@(sonora|example)(\.|$|\b)/i;

async function privacyViolations(page) {
  const { text, title } = await page.evaluate(() => ({
    text: document.body ? document.body.innerText || "" : "",
    title: document.title || "",
  }));
  const haystack = `${title}\n${text}`;
  const hits = [];
  for (const [re, why] of PRIVACY_DENY) {
    const m = haystack.match(re);
    if (m) hits.push(`${why}: "${m[0]}"`);
  }
  for (const email of haystack.match(EMAIL_RE) || []) {
    if (!EMAIL_ALLOW.test(email)) hits.push(`non-demo email address: "${email}"`);
  }
  return hits;
}

/**
 * The authoring surface a scene's `run` gets. Every call that matters to the cut
 * records a MARK stamped with the millisecond it happened, so stage 2 never has
 * to guess when to push the camera in, when to move the window or when a caption
 * should land: the edit is a by-product of performing the scene.
 */
function makeFilm(page, scene) {
  const t0 = Date.now();
  const at = () => Date.now() - t0;
  let cursorAt = { x: V.width * 0.5, y: V.height * 0.62 };

  async function boxOf(target) {
    if (target && typeof target === "object" && "x" in target) return target;
    const loc = typeof target === "string" ? page.locator(target).first() : target;
    await loc.waitFor({ state: "visible", timeout: 15_000 });
    const box = await loc.boundingBox();
    if (!box) throw new Error(`no box for ${target}`);
    return box;
  }

  const film = {
    at,

    /** Wall-clock hold. Footage keeps rolling; this is the pacing dial. */
    async wait(ms) {
      await sleep(ms);
    },

    async settle(ms = 700) {
      await page.evaluate(() => document.fonts && document.fonts.ready).catch(() => {});
      await sleep(ms);
    },

    async goto(path) {
      await page.goto(`${BASE}${path}`, { waitUntil: "networkidle" });
      // The drawn cursor lives in the DOM, so a navigation wipes it.
      await page.evaluate(
        ([x, y]) => window.__promoCursor.init(x, y),
        [cursorAt.x, cursorAt.y],
      );
      // Everything the screencast emitted up to here is the BLANK PAGE and the
      // navigation painting in. Mark the roll point so those frames are dropped.
      //
      // This is not theoretical. The screencast only emits on a paint, so a
      // static route emits very few frames — /dashboards produced six for a
      // seven-second take — and frame 00000 of that scene was solid black. With
      // so few samples the compositor holds that black across a real fraction of
      // the shot, and it lands exactly on the cross-fade in, where it reads as
      // the film stuttering. Filming starts when the app is on screen.
      scene.roll = at();
    },

    /**
     * Start the take HERE — everything captured before this point is dropped.
     *
     * `goto` sets a roll point of its own, which covers the blank page painting
     * in. This one is for a shot that needs to ARRIVE somewhere first: open a
     * board, switch to a tab, unlock a panel, and only then start filming. Put
     * the setup between `goto` and `roll`, and the film never sees it.
     */
    async roll(settle = 260) {
      await sleep(settle);
      scene.roll = at();
    },

    /**
     * Clear a field and type into it at a human-but-brisk rate.
     *
     * The hook's whole claim is "type while it listens", and the first cut of
     * this film never typed a character — the note body was fully written in
     * frame 0 and the scene was 99% unchanged frames. This is what makes the
     * shot honest AND what gives it its motion: a field filling in is the
     * product performing, which is worth more than any camera move.
     */
    async clearAndType(target, text, { cps = 22 } = {}) {
      await film.click(target, { after: 160 });
      await page.keyboard.press("Meta+a");
      await page.keyboard.press("Backspace");
      await sleep(200);
      const loc = typeof target === "string" ? page.locator(target).first() : target;
      await loc.type(text, { delay: 1000 / cps });
      await sleep(160);
    },

    async showCursor(on = true) {
      await page.evaluate((v) => window.__promoCursor.show(v), on);
      await sleep(240);
    },

    /**
     * Glide the drawn cursor to a target. The real mouse is moved at ~85% of the
     * way through, so hover states light up just as the pointer arrives instead
     * of a beat early (which reads as a glitch).
     */
    async cursorTo(target, { ms } = {}) {
      const box = await boxOf(target);
      const x = box.x + box.width / 2;
      const y = box.y + box.height / 2;
      const dist = Math.hypot(x - cursorAt.x, y - cursorAt.y);
      const dur = ms ?? Math.min(900, Math.max(320, 260 + dist * 0.55));
      await page.evaluate(([px, py, d]) => window.__promoCursor.moveTo(px, py, d), [x, y, dur]);
      await sleep(dur * 0.85);
      await page.mouse.move(x, y);
      await sleep(dur * 0.15);
      cursorAt = { x, y };
      return { x, y, box };
    },

    /** Move there, ripple, click. */
    async click(target, { ms, after = 420 } = {}) {
      const { x, y } = await film.cursorTo(target, { ms });
      await page.evaluate(() => window.__promoCursor.click());
      await sleep(90);
      await page.mouse.click(x, y);
      await sleep(after);
    },

    /** Type into a field at a human-but-brisk rate. */
    async type(target, text, { cps = 26, focusFirst = true } = {}) {
      if (focusFirst) await film.click(target, { after: 160 });
      const loc = typeof target === "string" ? page.locator(target).first() : target;
      await loc.type(text, { delay: 1000 / cps });
      await sleep(220);
    },

    /** Fire a mocked backend event (see mock-tauri.js `__demoEmit`). */
    async emit(event, payload) {
      await page.evaluate(([e, p]) => window.__demoEmit(e, p), [event, payload]);
    },

    /** Put the mocked backend into "recording" for `meetingId`. */
    async startRecording(meetingId = "m-atlas-roadmap") {
      await film.emit(EVENT_STATUS, { stage: "recording", message: "Recording…", meetingId });
    },

    async liveCaption(text) {
      await film.emit(EVENT_LIVE_CAPTION, { text });
    },

    /**
     * Camera. `target` is a selector, a rect, or null for "the whole screen".
     * A keyframe means: starting now, ease to this framing over `ms`.
     *
     * Aim at REGIONS, not at small controls near an edge. The compositor clamps
     * the frame to stay inside the app screen (compose.html `transformFor`), so
     * a target in a corner is only reachable at a deep zoom — ask for a modest
     * scale on an edge control and the camera will quietly settle mid-screen
     * instead. `bias` nudges the target vertically, which is the usual way to
     * keep a caption clear of what the shot is about.
     */
    async focus(target, { scale = 1.4, ms = 1100, bias = 0 } = {}) {
      let cx = V.width / 2;
      let cy = V.height / 2;
      if (target) {
        const box = await boxOf(target);
        cx = box.x + box.width / 2;
        cy = box.y + box.height / 2 + bias;
      }
      scene.camera.push({ t: at(), cx, cy, scale, ms });
    },

    /** Pull back out to the full screen. */
    async wide({ ms = 1100 } = {}) {
      scene.camera.push({ t: at(), cx: V.width / 2, cy: V.height / 2, scale: 1, ms });
    },

    /**
     * Scroll a target to the vertical middle of the app viewport before the
     * camera goes near it.
     *
     * This is not a nicety. Several of the panels worth filming — the action
     * items, the speaker timeline, the graph canvas — begin ABOVE 900 px and run
     * below it, and the compositor's camera clamp cannot leave the app screen.
     * So `focus` on a target whose centre is off the bottom silently settles
     * somewhere else and photographs the wrong thing, which is exactly how the
     * first cut ended up with a caption about a map over a picture of a list.
     * Scroll first, then frame what is now actually on screen.
     */
    async reveal(target, { settle = 500 } = {}) {
      const sel = typeof target === "string" ? target : null;
      if (!sel) return;
      await page.evaluate((s) => {
        const el = document.querySelector(s);
        if (!el) return;
        el.scrollIntoView({ block: "center", behavior: "instant" });
        const r = el.getBoundingClientRect();
        const delta = r.top + r.height / 2 - window.innerHeight / 2;
        if (Math.abs(delta) < 8) return;
        let n = el.parentElement;
        while (n && n !== document.body) {
          if (n.scrollHeight > n.clientHeight + 4) { n.scrollTop += delta; return; }
          n = n.parentElement;
        }
        window.scrollBy(0, delta);
      }, sel);
      await sleep(settle);
    },

    /**
     * Move the WINDOW ITSELF: one of compose.html's `LAYOUTS`.
     *
     *   full    the product, large and centred — the default, and the shot to
     *           use whenever the UI is the whole point
     *   lower   parked high, leaving a band underneath for a lower-third
     *   title   pushed right and tilted, bleeding off frame, with a type column
     *           on the LEFT — the layout for a claim, not for a feature
     *   titleR  mirrored: window bleeds left, type column on the RIGHT
     *   card    pushed back and down, for a full-frame chapter caption
     *
     * This is the film's cheapest source of motion and its main defence against
     * looking like a slideshow: the frame RE-COMPOSES between beats, so a cut
     * never lands on a picture that is the same shape as the one before it.
     */
    frame(mode, { ms = 900 } = {}) {
      scene.layout.push({ t: at(), mode, ms });
    },

    /**
     * A caption.
     *
     * `text` may mark accent words with asterisks: "Nothing leaves *your Mac*."
     * `kicker` is the small uppercase accent line above the headline — it is what
     * turns a line of type into a title rather than a subtitle, so most beats
     * should have one. `dur` is how long the caption stays up; stage 2 gives it a
     * rise-and-fade in and out, so the authored time is when it should be READ.
     */
    title(text, { kicker = "", sub = "", dur = 2800, kind = "lower", align, rule } = {}) {
      scene.captions.push({ t: at(), text, kicker, sub, dur, kind, align, rule });
    },

    /**
     * Ring a REGION of the UI and shade everything else down.
     *
     * This is how the re-cut shows a feature without spending a push-in on it:
     * the wide shot keeps its context, and the eye still goes exactly where the
     * film is pointing. Cheaper than a camera move, and it can point at
     * something small near an edge, which the clamped camera cannot.
     */
    async spotlight(target, { dur = 1700, label = "", pad = 10 } = {}) {
      const b = await boxOf(target);
      scene.spots.push({
        t: at(),
        dur,
        label,
        x: b.x - pad,
        y: b.y - pad,
        width: b.width + pad * 2,
        height: b.height + pad * 2,
      });
    },

    /** Scan what is currently rendered before it is committed to footage. */
    async assertClean(where) {
      const leaks = await privacyViolations(page);
      if (leaks.length) throw new Error(`PRIVACY: ${where}: ${leaks.join("; ")}`);
    },
  };
  return film;
}

// ── The cut ─────────────────────────────────────────────────────────────────
//
// EIGHTEEN SHOTS, and the rules they follow are measured rather than felt.
//
// The previous cut was nine scenes with ZERO hard cuts, 34 camera moves against
// 16 things the app actually did, and 52% of its running time with the product
// doing nothing at all. It papered over that with a continuous camera drift —
// which put the film in the 1–15 px/s "visible but purposeless" band 64% of the
// time, where no reference product film spends more than 28%. See
// `docs/research/2026-09-01-promo-cut-rhythm.md`.
//
// So, three binding rules, and the shots below are just their consequence:
//
//   1. MOTION COMES FROM THE APP. Every shot performs something — a field being
//      typed, a tab switching, a tree expanding, a panel refusing to open. The
//      camera is LOCKED unless it is following a click, and then it travels
//      once and stops.
//   2. THE CAPTION RUNS WITH THE ACTION, NOT AFTER IT. Words and pictures at
//      the same time (Mayer's temporal contiguity, 8/8 tests, d = 1.31) — which
//      also means the caption's reading time is paid for by the action's
//      duration instead of by a hold. That is what removes the dead air.
//   3. CUT WHEN THE SHOT STOPS EARNING ITS PLACE. Eleven hard cuts inside the
//      acts, softer transitions between them. ASL ~5 s.
//
// Caption budget, from Netflix (20 cps, ≤84 chars on screen, ≤7 s) and BBC
// (160–180 wpm), which converge on ~0.33 s per word:
//
//      dur ≈ 0.33 × words + 0.6      (the 0.6 covers the per-word stagger)
//
// counting kicker + headline + sub together. Every `dur:` below was computed
// that way; the previous cut had 15 of 15 captions under their own read time,
// a 56-second collective deficit, which is the actual reason it had to sit and
// stare.
//
// Every selector is against the CURRENT shell. Re-run `record` after any shell
// change rather than trusting that the takes still land.
const SCENES = {
  /* ═══ ACT 1 — CAPTURE ═══════════════════════════════════════════════════ */

  /* 1. The hook. The note is CLEARED and typed on camera, because the caption
   *    promises exactly that — and the previous cut's version of this shot was
   *    99% unchanged frames with the note fully written in frame 0, under the
   *    words "Type while it listens". The typing is both the honesty fix and
   *    the entire motion budget of the shot. */
  hook: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/record");
      await film.startRecording();
      await film.settle(500);
      await film.roll();
      await film.assertClean("hook");

      film.frame("title", { ms: 10 });
      film.title("Nothing leaves|*your Mac*.", {
        kicker: "On-device",
        sub: "Recorded and transcribed right here.",
        dur: 3900,
        kind: "title",
      });
      await film.liveCaption("…so the sync-layer dependency is gone — Atlas is unblocked for GA.");
      await film.wait(600);
      await film.clearAndType("app-meeting-conversation textarea", "Kickoff ran long — good energy on the GA date.", { cps: 21 });
      await film.wait(500);
      await film.liveCaption("Then let's commit to the 14th. Priya, you own the migration note.");
      await film.wait(1100);
    },
  },

  /* 2. The live ticker — your side, transcribed as you speak. */
  hookLive: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/record");
      await film.startRecording();
      await film.liveCaption("Marcus — Windows loopback is the last blocker on the list.");
      await film.settle(500);
      await film.roll();
      await film.assertClean("hookLive");

      film.frame("lower", { ms: 10 });
      film.title("Your side, *as you speak*.", { kicker: "Live", dur: 2600 });
      await film.spotlight(".rec-foot", { dur: 2300, label: "Full transcript after Stop", pad: 6 });
      await film.wait(1300);
      await film.liveCaption("And the design-partner track is committed for the 15th.");
      await film.wait(1900);
    },
  },

  /* ═══ ACT 2 — WHAT IS WAITING AFTERWARDS ════════════════════════════════ */

  /* 3. The note itself. One motivated push into the header, then a full stop. */
  note: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/meeting/m-atlas-roadmap");
      await film.settle(700);
      await film.roll();
      await film.assertClean("note");

      film.frame("lower", { ms: 10 });
      film.title("A note you'd have|*written yourself*.", {
        kicker: "Minutes later",
        dur: 3200,
      });
      await film.wait(700);
      await film.wait(2400);
    },
  },

  /* 4. Related — and then CLICK one, so the link is a thing that happens rather
   *    than a thing that is described. The shot lands on the linked note. */
  noteRelated: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/meeting/m-atlas-roadmap");
      await film.settle(700);
      await film.roll();
      await film.assertClean("noteRelated");

      film.frame("lower", { ms: 10 });
      film.title("Linked to *what it touches*.", { kicker: "Related", dur: 2600 });
      await film.spotlight(".related-primary", { dur: 1900 });
      await film.showCursor(true);
      await film.wait(1600);
      await film.click('.related-primary :text("Atlas — PRD v3")', { after: 900 });
      await film.showCursor(false);
      await film.assertClean("noteRelated-landed");
      await film.wait(250);
    },
  },

  /* 5. Action items. The scroll IS the action — they start at y≈637 and run past
   *    the fold, so revealing them is both the reframe and the motion. */
  noteActions: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/meeting/m-atlas-roadmap");
      await film.settle(700);
      await film.roll();

      film.frame("lower", { ms: 10 });
      film.title("Owners. Dates. *Done.*", {
        kicker: "Action items",
        sub: "Into Apple Reminders or Obsidian Tasks.",
        dur: 4200,
      });
      await film.wait(500);
      await film.reveal("app-meeting-actions");
      await film.assertClean("noteActions");
      await film.wait(3100);
    },
  },

  /* ═══ ACT 3 — PROOF ═════════════════════════════════════════════════════ */

  /* 6. Dual-stream. The tab click is the action; the timeline paints in. */
  speakers: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/meeting/m-atlas-roadmap");
      await film.settle(600);
      await film.roll();

      film.frame("lower", { ms: 10 });
      film.title("Your mic and the room,|*on separate tracks*.", {
        kicker: "Dual-stream",
        dur: 3600,
      });
      await film.showCursor(true);
      await film.click("button:has-text('Audio')", { after: 900 });
      await film.showCursor(false);
      await film.assertClean("speakers");
      await film.reveal("app-meeting-timeline");
      await film.wait(1200);
    },
  },

  /* 7. Topics — a chip click jumps the timeline. Setup happens BEFORE the roll,
   *    so the film never sees the tab being opened a second time. */
  speakersTopics: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/meeting/m-atlas-roadmap");
      await page.locator("button:has-text('Audio')").first().click();
      await film.settle(900);
      await film.reveal("app-meeting-timeline");
      await film.roll();
      await film.assertClean("speakersTopics");

      film.frame("lower", { ms: 10 });
      film.title("Jump to *any moment*.", { kicker: "Topics", dur: 2300 });
      await film.showCursor(true);
      await film.click(':text("Mobile redesign scope")', { after: 900 });
      await film.showCursor(false);
      await film.wait(400);
    },
  },

  /* 8. Ask — typed live at 17 cps, because this scene captures at ~19 fps and at
   *    30 cps three characters land between frames and it reads as a stutter. */
  askType: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/ask");
      await film.settle(600);
      await film.roll();
      await film.assertClean("askType");

      film.frame("lower", { ms: 10 });
      film.title("Ask across|*everything you've recorded*.", { kicker: "Ask", dur: 2600 });
      await film.showCursor(true);
      await film.type("textarea, input[type=text]", "What did we decide about Atlas, and who owns what's left?", { cps: 17 });
      await film.showCursor(false);
      await film.wait(700);
    },
  },

  /* 9. The answer streaming in, with its sources ringed. The question is already
   *    typed before the roll — this shot is only the answer arriving. */
  askAnswer: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/ask");
      await film.settle(500);
      await page.locator("textarea, input[type=text]").first().fill("What did we decide about Atlas, and who owns what's left?");
      await film.roll();

      await page.keyboard.press("Enter");
      film.frame("lower", { ms: 10 });
      film.title("It shows *its receipts*.", { kicker: "Grounded", dur: 2300 });
      await film.wait(1900);
      await film.assertClean("askAnswer");
      await film.spotlight("app-sources.ask-sources", { dur: 2100, label: "Where it came from" });
      await film.wait(2500);
    },
  },

  /* ═══ ACT 4 — THE MAP ═══════════════════════════════════════════════════ */

  /* 10. The force layout settles on camera — the one shot whose motion needs no
   *     help at all. NOT `/graph`: that route is the entity BROWSER and
   *     photographs as a list of chips, which would make the caption a lie. */
  brain: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/brain");
      await film.settle(600);
      await page.locator("button:has-text('Full brain graph')").first().click();
      await page.waitForSelector("app-full-brain-graph", { timeout: 15_000 });
      const clusters = page.locator("button", { hasText: /^\s*Clusters\s*$/ }).first();
      if (await clusters.count()) await clusters.click();
      await film.reveal("app-full-brain-graph canvas", { settle: 300 });
      await film.roll();
      await film.assertClean("brain");

      film.frame("titleR", { ms: 10 });
      // `kind` MUST match the frame mode: the layout puts the window off to one
      // side, and a caption left on the default `lower` lands a bottom-left
      // third straight across it. Same bug class as the centred-window one —
      // two halves of one composition authored in two places.
      film.title("Meetings, notes, documents,|people — *one brain*.", {
        kicker: "It connects itself",
        dur: 3900,
        kind: "titleR",
      });
      const fit = page.locator("button", { hasText: /^\s*Fit\s*$/ }).first();
      if (await fit.count()) await fit.click();
      await film.wait(5400);
    },
  },

  /* ═══ ACT 5 — ORGANISE ══════════════════════════════════════════════════ */

  /* 11. The Workspaces tree. Expanding a node is the action — and it navigates,
   *     so the whole right-hand pane changes with it. */
  workspace: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/notes");
      await film.settle(700);
      await film.roll();
      await film.assertClean("workspace");

      film.frame("lower", { ms: 10 });
      film.title("Your vault, *the way you think*.", { kicker: "Workspaces", dur: 2900 });
      await film.spotlight('[aria-label="Workspaces"] .sb-tree, mur-sidebar .sb-workspaces', {
        dur: 1800,
        label: "3 unfiled recordings",
      });
      await film.showCursor(true);
      await film.wait(1500);
      await film.click('mur-sidebar :text-is("Product")', { after: 1000 });
      await film.showCursor(false);
      await film.wait(250);
    },
  },

  /* 12. Collapse to the rail and back — the app animating its own chrome. */
  workspaceRail: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/notes");
      await film.settle(600);
      await film.roll();

      film.frame("lower", { ms: 10 });
      film.title("Collapse it to *a rail*.", { kicker: "One click", dur: 2900 });
      await film.showCursor(true);
      await film.click('button[aria-label="Collapse sidebar"]', { after: 1300 });
      await film.click('button[aria-label="Expand sidebar"]', { after: 900 });
      await film.showCursor(false);
      await film.wait(250);
    },
  },

  /* 13. ⌘K — the palette opens and takes a query. */
  search: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/notes");
      await film.settle(600);
      await film.roll();

      film.frame("lower", { ms: 10 });
      film.title("Search everything.|*On device.*", { kicker: "⌘K", dur: 2300 });
      await page.keyboard.press("Meta+k");
      await film.wait(700);
      await film.assertClean("search");
      await film.type("mur-quick-search input", "atlas", { cps: 9, focusFirst: false });
      await film.wait(1800);
    },
  },

  /* ═══ ACT 6 — SURFACES ══════════════════════════════════════════════════ */

  /* 14. Opening a board. */
  boards: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/dashboards");
      await film.settle(700);
      await film.roll();
      await film.assertClean("boards");

      film.frame("lower", { ms: 10 });
      film.title("Compose a board. The brain|*reasons inside it*.", {
        kicker: "Dashboards",
        dur: 3600,
      });
      await film.showCursor(true);
      await film.wait(900);
      // Scoped to the main column: the sidebar carries the same board title and
      // CSS truncation leaves the full string in the DOM, so an unscoped match
      // resolves to the tree row instead of the card.
      await film.click('.main-col :text-is("Atlas — GA readiness")', { after: 1100 });
      await film.showCursor(false);
      await film.assertClean("boards-detail");
      await film.wait(700);
    },
  },

  /* 15. Its standing answer, and the tab that shows what is owed. */
  boardsTabs: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/dashboards/d-atlas");
      await film.settle(800);
      await film.roll();
      await film.assertClean("boardsTabs");

      film.frame("lower", { ms: 10 });
      film.title("It answers, and|*shows its working*.", { kicker: "Brief", dur: 2900 });
      await film.wait(1500);
      await film.showCursor(true);
      await film.click('button:has-text("Commitments")', { after: 1000 });
      await film.showCursor(false);
      await film.wait(300);
    },
  },

  /* 16. People — the cursor glide is the action, and it lights the hover states
   *     on the way. */
  people: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/people");
      await film.settle(600);
      await film.roll();
      await film.assertClean("people");

      film.frame("lower", { ms: 10 });
      film.title("It remembers *who owes what*.", { kicker: "People", dur: 2600 });
      await film.showCursor(true);
      await film.cursorTo("text=Sarah Chen", { ms: 620 });
      await film.wait(320);
      await film.cursorTo("text=Marcus Reid", { ms: 560 });
      await film.wait(300);
      await film.cursorTo("text=Priya Nair", { ms: 640 });
      await film.showCursor(false);
      await film.wait(250);
    },
  },

  /* ═══ ACT 7 — OWNERSHIP ═════════════════════════════════════════════════ */

  /* 17. The contrast shot, and the reason this act works at all: open a
   *     workspace that HAS contents, then click the sealed one and watch it
   *     refuse. The refusal only means something once you have just seen what
   *     an open workspace looks like. */
  lockRefuse: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/container/f-eng");
      await film.settle(700);
      await film.roll();
      await film.assertClean("lockRefuse-open");

      film.frame("lower", { ms: 10 });
      film.title("Locked means *locked*.", {
        kicker: "Sealed",
        sub: "Even to Murmur.",
        dur: 2900,
      });
      await film.showCursor(true);
      await film.wait(900);
      await film.click('mur-sidebar :text-is("Personal")', { after: 1200 });
      await film.showCursor(false);
      await film.assertClean("lockRefuse-sealed");
      await film.wait(1300);
    },
  },

  /* 18. The claim, on its own shot. A frame mode NEVER animates inside a shot —
   *     that is a slide, and a slide is the thing this cut exists to remove. If
   *     the composition needs to change, that is what a cut is for. */
  lockClaim: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/container/f-personal");
      await film.settle(600);
      await film.roll();

      film.frame("title", { ms: 10 });
      film.title("A door *only you*|hold the key to.", {
        kicker: "Lock a workspace",
        dur: 4200,
        kind: "title",
      });
      await film.wait(4550);
    },
  },

  /* 18. And the files are yours regardless. */
  lockFiles: {
    transition: "cut",
    async run(page, film) {
      await film.goto("/container/f-personal");
      await film.settle(600);
      await film.roll();

      film.frame("lower", { ms: 10 });
      film.title("And it is all *plain Markdown*.", {
        kicker: "Your files",
        sub: "In your own Obsidian vault.",
        dur: 4900,
      });
      await film.showCursor(true);
      await film.cursorTo('mur-sidebar :text-is("Personal")', { ms: 800 });
      await film.showCursor(false);
      await film.wait(3500);
    },
  },
};

/**
 * Fail the run if the footage came back at 1×.
 *
 * A soft video is not an error anywhere — it renders, it encodes, it uploads,
 * and the only symptom is that it looks slightly cheap. That is precisely the
 * kind of regression that survives for releases, so the frame size is checked
 * rather than trusted. Reads the JPEG SOF0 header directly; no image library.
 */
async function assertSupersampled(dir, scene) {
  if (!scene.frames.length) return;
  const buf = readFileSync(join(dir, scene.frames[0].file));
  let i = 2;
  while (i < buf.length - 9) {
    if (buf[i] !== 0xff) { i++; continue; }
    const marker = buf[i + 1];
    // SOF0..SOF3 / SOF5..SOF7 / SOF9..SOF11 / SOF13..SOF15 carry the dimensions.
    if (marker >= 0xc0 && marker <= 0xcf && ![0xc4, 0xc8, 0xcc].includes(marker)) {
      const h = buf.readUInt16BE(i + 5);
      const w = buf.readUInt16BE(i + 7);
      if (w !== V.width * SCALE || h !== V.height * SCALE) {
        throw new Error(
          `${scene.id}: footage is ${w}×${h}, expected ${V.width * SCALE}×${V.height * SCALE} — ` +
            `the --force-device-scale-factor=${SCALE} launch flag is not taking effect, so every ` +
            `camera push-in would upscale`,
        );
      }
      return;
    }
    i += 2 + buf.readUInt16BE(i + 2);
  }
  throw new Error(`${scene.id}: could not read the JPEG header to verify the capture scale`);
}

async function recordScene(browser, name, dir) {
  const scene = {
    id: name,
    transition: SCENES[name].transition || "dissolve",
    camera: [],
    layout: [],
    captions: [],
    spots: [],
    frames: [],
  };
  const ctx = await browser.newContext({
    viewport: V,
    deviceScaleFactor: 2,
    colorScheme: "dark",
    locale: "en-US",
  });
  const page = await ctx.newPage();
  const consoleErrors = [];
  page.on("console", (m) => {
    if (m.type() === "error") consoleErrors.push(m.text());
  });

  await page.addInitScript("window.__demoRich = true;");
  await page.addInitScript(`window.__demoVersion = ${JSON.stringify(VERSION)};`);
  await page.addInitScript(MOCK);
  await page.addInitScript(CURSOR);

  mkdirSync(dir, { recursive: true });

  const cdp = await ctx.newCDPSession(page);
  let capturing = false;
  let t0 = 0;
  const writes = [];

  cdp.on("Page.screencastFrame", (params) => {
    // Ack FIRST and unconditionally — the browser stops sending frames until the
    // previous one is acknowledged, so a slow write must not stall the stream.
    cdp.send("Page.screencastFrameAck", { sessionId: params.sessionId }).catch(() => {});
    if (!capturing) return;
    const n = scene.frames.length;
    const file = `${String(n).padStart(5, "0")}.jpg`;
    const ts = params.metadata && params.metadata.timestamp ? params.metadata.timestamp * 1000 : Date.now();
    scene.frames.push({ t: Math.max(0, ts - t0), file });
    writes.push(writeFile(join(dir, file), Buffer.from(params.data, "base64")));
  });

  await cdp.send("Page.startScreencast", {
    format: "jpeg",
    quality: 86,
    maxWidth: V.width * SCALE,
    maxHeight: V.height * SCALE,
    everyNthFrame: 1,
  });

  const film = makeFilm(page, scene);
  t0 = Date.now();
  capturing = true;
  try {
    await SCENES[name].run(page, film);
    // A short tail so the last camera move and caption have somewhere to land.
    await sleep(500);
    scene.dur = Date.now() - t0;
  } finally {
    capturing = false;
    await cdp.send("Page.stopScreencast").catch(() => {});
    await Promise.all(writes);
    await ctx.close();
  }

  await assertSupersampled(dir, scene);

  /*
   * The film clock starts when the app is actually on screen.
   *
   * Two things have to be trimmed off the front of a take. `scene.roll` is the
   * moment `goto` finished, so everything before it is the blank page and the
   * navigation painting in — drop those frames outright. After that, a page that
   * paints late would still open on a hold of nothing, so the clock starts at
   * whichever came later, the roll or the first surviving frame.
   *
   * The guard matters for a static route: /dashboards, /people and
   * /container/:id repaint so rarely that the settled page can be the LAST
   * pre-roll frame with nothing after it, and filtering strictly would leave the
   * scene with no footage at all.
   */
  const roll = Math.max(0, scene.roll || 0);
  if (roll > 0 && scene.frames.length) {
    const before = scene.frames.filter((f) => f.t < roll);
    const after = scene.frames.filter((f) => f.t >= roll);
    // SEED the take with the last pre-roll frame, re-stamped to the roll instant.
    //
    // That frame is the settled app — `goto` waits for networkidle, so the page
    // has painted by the time the roll is stamped — and it must be kept even
    // when there are plenty of later frames. A static route does not repaint
    // until something drives it, which on /container/:id is a cursor glide five
    // seconds in; dropping everything before that made the first surviving frame
    // the clock origin and silently deleted the whole opening beat, taking a
    // ten-second scene down to five.
    const seed = before.length ? [Object.assign({}, before[before.length - 1], { t: roll })] : [];
    scene.frames = seed.concat(after);
  }
  const skew = roll;
  for (const f of scene.frames) f.t = Math.max(0, f.t - skew);
  for (const c of scene.camera) c.t = Math.max(0, c.t - skew);
  for (const c of scene.layout) c.t = Math.max(0, c.t - skew);
  for (const c of scene.captions) c.t = Math.max(0, c.t - skew);
  for (const c of scene.spots) c.t = Math.max(0, c.t - skew);
  scene.dur -= skew;
  scene.consoleErrors = consoleErrors;
  return scene;
}

async function main() {
  const wanted = process.argv.slice(2);
  const names = wanted.length ? wanted : Object.keys(SCENES);
  for (const n of names) {
    if (!SCENES[n]) throw new Error(`unknown scene: ${n}`);
  }

  // A partial re-record keeps the scenes it is not re-shooting: re-taking one
  // beat should not cost the other eight, and the manifest is merged below.
  const partial = wanted.length > 0;
  let previous = [];
  if (partial) {
    try {
      previous = JSON.parse(readFileSync(join(OUT, "manifest.json"), "utf8")).scenes || [];
    } catch {
      previous = [];
    }
  }
  if (!partial) {
    rmSync(join(OUT, "frames"), { recursive: true, force: true });
  }
  mkdirSync(join(OUT, "frames"), { recursive: true });

  const browser = await chromium.launch({ args: launchArgs, headless: HEADLESS });
  const fresh = new Map();
  try {
    for (const name of names) {
      const dir = join(OUT, "frames", name);
      rmSync(dir, { recursive: true, force: true });
      process.stdout.write(`▸ ${name} … `);
      const scene = await recordScene(browser, name, dir);
      fresh.set(name, scene);
      const secs = (scene.dur / 1000).toFixed(1);
      const fps = (scene.frames.length / (scene.dur / 1000)).toFixed(0);
      console.log(`${scene.frames.length} frames / ${secs}s (~${fps}fps)`);
      // Print the TEXT, not a count. A bare "1 console error" is noise nobody
      // acts on; the message is either harmless or it is the reason a panel
      // rendered empty in the footage.
      for (const e of scene.consoleErrors.slice(0, 3)) console.log(`    ⚠ ${e.split("\n")[0].slice(0, 160)}`);
    }
  } finally {
    await browser.close();
  }

  // Order is always the SCENES order, never the argv order — a partial re-record
  // must not silently re-cut the film.
  const scenes = Object.keys(SCENES)
    .map((n) => fresh.get(n) || previous.find((p) => p.id === n))
    .filter(Boolean);

  const manifest = { version: VERSION, viewport: V, scenes };
  writeFileSync(join(OUT, "manifest.json"), JSON.stringify(manifest, null, 2));
  const total = scenes.reduce((a, s) => a + s.dur, 0) / 1000;
  console.log(`\n${scenes.length} scenes · ${total.toFixed(1)}s of footage → ${OUT}`);
}

main().catch((e) => {
  console.error(`\n✗ ${e.message}`);
  process.exit(1);
});
