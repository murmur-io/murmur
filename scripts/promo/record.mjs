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
// Beat order follows the brief: hook → output → proof → intelligence →
// organisation → surfaces → ownership → CTA. Each scene is one continuous take;
// stage 2 joins them with the transition each scene declares.
//
// Every selector here is against the CURRENT shell (`mur-sidebar.primary-sidebar`
// and the Workspaces/Shared tree). The previous cut still aimed at
// `nav.global-rail`, which the 2026-08-31 sidebar rewrite deleted — a scene file
// silently describing a UI that no longer exists is the failure mode this whole
// harness is supposed to make impossible, so re-run `record` after any shell
// change rather than trusting that the takes still land.
const SCENES = {
  /* 1. HOOK — open on the product mid-action. No logo, no title card.
   *
   * The cold open is the whole ballgame for a technical audience: the first
   * three seconds decide whether the rest gets watched, and the strongest hook
   * is the software visibly doing the thing. So: already recording, timer
   * running, a note being kept, and captions arriving. */
  capture: {
    transition: "dissolve",
    async run(page, film) {
      await film.goto("/record");
      await film.settle(900);
      await film.startRecording();
      await film.wait(500);
      await film.liveCaption("…so the sync-layer dependency is gone — Atlas is unblocked for GA.");
      await film.wait(900);
      await film.assertClean("capture");

      // Beat 1 — let the product play alone for a beat before saying anything.
      await film.wait(400);

      // Beat 2 — the claim, in the title layout: window tilted away to the
      // right, type holding the left third. This is the shot the old cut could
      // not make, because its window never moved.
      film.frame("title", { ms: 1150 });
      // ACCURACY, not modesty: an earlier draft of this line said "recording,
      // transcription AND REASONING run locally". `DEFAULT_PROVIDER_ID` is
      // `claude_code` (settings/config.rs calls it "the default brain", and it
      // is cloud-classified), and this very scene photographs a `claude-opus-4-8`
      // model chip — so that caption would have been a lie by framing, over
      // footage that disproved it. What IS unconditionally true is the promise in
      // CLAUDE.md's first non-negotiable constraint: audio and transcript stay on
      // the device. Claim exactly that.
      film.title("Nothing leaves|*your Mac*.", {
        kicker: "On-device",
        sub: "Recorded and transcribed on this Mac. Your audio and transcript never leave it. No account, no upload.",
        dur: 3600,
        kind: "title",
      });
      await film.wait(4150);

      // Beat 3 — re-compose to the lower band and go to work: the note being
      // kept while the meeting runs, then the live caption ticker.
      film.frame("lower", { ms: 950 });
      await film.liveCaption("Then let's commit to the 14th. Priya, you own the migration note.");
      await film.focus("app-meeting-conversation", { scale: 1.5, ms: 1400, bias: -40 });
      film.title("Type while it *listens*.", {
        kicker: "Companion note",
        sub: "What you jot down steers the summary it writes afterwards.",
        dur: 2900,
      });
      await film.wait(3300);

      await film.wide({ ms: 900 });
      await film.spotlight(".rec-foot", { dur: 2200, label: "Live · your side", pad: 6 });
      await film.liveCaption("Marcus — Windows loopback is the last blocker on the list.");
      await film.wait(2300);
    },
  },

  /* 2. THE OUTPUT — what is waiting when the meeting ends. */
  note: {
    transition: "push",
    async run(page, film) {
      await film.goto("/meeting/m-atlas-roadmap");
      await film.settle(900);
      await film.assertClean("note");

      film.frame("lower", { ms: 10 });
      film.title("A note you'd have *written yourself*.", {
        kicker: "Minutes later",
        sub: "Titled, tagged, filed into the right workspace — and exported to your vault as plain Markdown.",
        dur: 3400,
      });
      await film.wait(1500);
      await film.spotlight(".related-primary", { dur: 2200, label: "Linked to what it touches" });
      await film.wait(2300);

      // Say it wide, THEN push in — a caption authored after the push lands on
      // top of whatever the push-in filled the frame with, and the action-items
      // panel is exactly the kind of dense content that turns into mush under
      // 46 px type. The action items are the beat that closes a sale: they are
      // the part a human would otherwise re-type by hand. They start at y≈637
      // and run past the fold, so scroll them into the middle before framing.
      film.title("Owners. Dates. *Done.*", {
        kicker: "Action items",
        sub: "Straight into Apple Reminders or Obsidian Tasks.",
        dur: 2800,
      });
      await film.wait(1200);
      await film.reveal("app-meeting-actions");
      await film.focus("app-meeting-actions", { scale: 1.45, ms: 1300 });
      await film.wait(2400);
      await film.wide({ ms: 900 });
      await film.wait(400);
    },
  },

  /* 3. PROOF — the dual-stream transcript, the thing that is actually hard.
   *
   * Starts on the Note tab and CLICKS across, so the beat has the app's own
   * motion in it rather than a camera move over a still panel. */
  speakers: {
    transition: "dissolve",
    async run(page, film) {
      await film.goto("/meeting/m-atlas-roadmap");
      await film.settle(800);
      await film.showCursor(true);
      film.frame("lower", { ms: 10 });
      await film.click("button:has-text('Audio')", { after: 1100 });
      await film.showCursor(false);
      await film.assertClean("speakers");

      film.title("Your mic and the room, *on separate tracks*.", {
        kicker: "Dual-stream",
        sub: "whisper.cpp on Metal transcribes each side on its own, then merges them by wall clock.",
        dur: 3400,
      });
      await film.wait(1200);
      await film.reveal("app-meeting-timeline");
      await film.focus("app-meeting-timeline", { scale: 1.4, ms: 1500 });
      await film.wait(2900);
      await film.wide({ ms: 950 });
      await film.wait(400);
    },
  },

  /* 4. PROOF — ask across everything, with citations. */
  ask: {
    transition: "push",
    async run(page, film) {
      await film.goto("/ask");
      await film.settle(700);
      await film.showCursor(true);
      film.frame("lower", { ms: 10 });
      film.title("Ask across *everything you've recorded*.", {
        kicker: "Ask",
        sub: "Forty-seven meetings, every note, every document you added.",
        dur: 3200,
      });
      // 17 cps, not 26: this scene captures at ~19 fps, and at 30 cps three
      // characters land between frames and it reads as a stutter, not as typing.
      await film.type("textarea, input[type=text]", "What did we decide about Atlas, and who owns what's left?", { cps: 17 });
      await film.wait(320);
      await page.keyboard.press("Enter");
      await film.wait(1700);
      await film.showCursor(false);
      await film.assertClean("ask");

      await film.focus({ x: 300, y: 200, width: 1000, height: 380 }, { scale: 1.42, ms: 1300 });
      await film.wait(2000);
      await film.wide({ ms: 900 });
      // Not "every claim, cited": this row cites the ANSWER, with a meeting and
      // a date per chip. Murmur does have per-claim receipts elsewhere, but a
      // label has to be true of the pixels it is drawn on.
      await film.spotlight("app-sources.ask-sources", { dur: 2400, label: "Where it came from" });
      // "…and the second of audio it came from" overclaims what this shot shows:
      // the SOURCES chips carry a meeting and a date, not a timestamp. Claim the
      // thing that is on screen — which is also what the route's own copy says.
      film.title("It shows *its receipts*.", {
        kicker: "Grounded",
        sub: "Every answer links back to the meetings it came from.",
        dur: 2900,
      });
      await film.wait(3300);
    },
  },

  /* 5. THE MAP — motion for free: the force layout settles on camera.
   *
   * NOT `/graph`. That route is the entity BROWSER — People and Projects as
   * chips — which photographs as a list and flatly fails to deliver the
   * caption's promise. The actual map lives behind the "Full brain graph"
   * disclosure on `/brain`, which is where the screenshot harness gets it too. */
  brain: {
    transition: "whip",
    async run(page, film) {
      await film.goto("/brain");
      await film.settle(900);
      await film.showCursor(true);
      film.frame("full", { ms: 10 });
      await film.click("button:has-text('Full brain graph')", { after: 700 });
      await page.waitForSelector("app-full-brain-graph", { timeout: 15_000 });
      await film.showCursor(false);
      // "Clusters" packs the map by community; "Layers" leaves most of the
      // canvas empty at this size. "Fit" then centres it.
      const clusters = page.locator("button", { hasText: /^\s*Clusters\s*$/ }).first();
      if (await clusters.count()) await clusters.click();
      await film.wait(1200);
      const fit = page.locator("button", { hasText: /^\s*Fit\s*$/ }).first();
      if (await fit.count()) await fit.click();
      // Park the CANVAS in frame, not the component host. The host starts at the
      // legend and control rows, so centring it leaves the map itself below the
      // fold — which is what the first take photographed.
      await film.reveal("app-full-brain-graph canvas", { settle: 800 });
      await film.assertClean("brain");

      film.frame("titleR", { ms: 1150 });
      film.title("Meetings, notes, documents,|people — *one brain*.", {
        kicker: "It connects itself",
        sub: "Co-occurrence, mentions, wikilinks and semantic neighbours, drawn as one map.",
        dur: 3600,
        kind: "titleR",
      });
      await film.wait(3980);
      film.frame("full", { ms: 1000 });
      // A slow, continuous push on the map. The force layout is still settling,
      // so this beat has motion of its own — the camera only frames it.
      await film.focus("app-full-brain-graph canvas", { scale: 1.28, ms: 2800 });
      await film.wait(2500);
    },
  },

  /* 6. ORGANISATION — the sidebar, which is the shell rewrite's whole point.
   *
   * The collapse/expand is REAL UI animation, not a camera move: the rail
   * animates its own width and swaps to icons. Filming the app's own motion is
   * always worth more than manufacturing some. */
  workspace: {
    transition: "pushUp",
    async run(page, film) {
      await film.goto("/notes");
      await film.settle(900);
      await film.assertClean("workspace");

      film.frame("lower", { ms: 10 });
      film.title("Your vault, *the way you think*.", {
        kicker: "Workspaces",
        sub: "Projects, clients, shared brains — and an inbox for anything you haven't filed yet.",
        dur: 3300,
      });
      await film.wait(1200);
      await film.spotlight('[aria-label="Workspaces"] .sb-tree, mur-sidebar .sb-workspaces', {
        dur: 2400,
        label: "3 unfiled recordings",
      });
      await film.wait(2760);

      // Collapse to the icon rail and back — the app animating itself.
      await film.showCursor(true);
      await film.focus({ x: 220, y: 450, width: 620, height: 620 }, { scale: 1.34, ms: 900 });
      await film.click('button[aria-label="Collapse sidebar"]', { after: 1150 });
      // Not "one keystroke": `toggleSidebar()` is click-only, there is no
      // accelerator bound to it.
      film.title("Collapse it to a rail. *Get out of the way.*", {
        kicker: "One click",
        dur: 2400,
      });
      await film.wait(1500);
      await film.click('button[aria-label="Expand sidebar"]', { after: 1300 });
      await film.showCursor(false);
      await film.wide({ ms: 900 });
      await film.wait(500);
    },
  },

  /* 7. SURFACES — a board is where the brain's reasoning is standing output
   *    rather than a chat answer, which is the least like every other notes app
   *    this product gets.
   *
   *    The click into the board is doing two jobs: it is the beat, and it is the
   *    FOOTAGE. `/dashboards` is a static route — the screencast only emits on a
   *    paint, so an un-driven take of it captured a single frame for seven
   *    seconds. Driving the UI is what gives these scenes anything to sample. */
  boards: {
    transition: "push",
    async run(page, film) {
      await film.goto("/dashboards");
      await film.settle(800);
      await film.assertClean("boards");

      film.frame("lower", { ms: 10 });
      film.title("Compose a board. The brain *reasons inside it*.", {
        kicker: "Dashboards",
        sub: "Tiles that stay live against your vault.",
        dur: 2800,
      });
      await film.showCursor(true);
      await film.wait(1300);
      // Scoped to the main column: the sidebar carries the same board title, and
      // CSS truncation leaves the full string in the DOM, so an unscoped text
      // match resolves to the tree row instead of the card.
      await film.click('.main-col :text-is("Atlas — GA readiness")', { after: 1400 });
      await film.showCursor(false);
      await film.assertClean("board-detail");

      film.title("It answers, and *shows its working*.", {
        kicker: "Brief",
        sub: "A standing answer, what needs attention, and the evidence underneath it.",
        dur: 2600,
      });
      await film.wait(1400);
      await film.focus({ x: 300, y: 205, width: 1260, height: 290 }, { scale: 1.3, ms: 1400 });
      await film.wait(2200);
      await film.wide({ ms: 850 });
      await film.wait(350);
    },
  },

  /* 8. MEMORY — the human half. Short, because the claim is small and obvious
   *    once photographed. The cursor glide is deliberate: it lights the hover
   *    states AND it is the only thing repainting this route. */
  people: {
    transition: "dissolve",
    async run(page, film) {
      await film.goto("/people");
      await film.settle(800);
      await film.assertClean("people");

      film.frame("lower", { ms: 10 });
      film.title("It remembers *who owes what*.", {
        kicker: "People",
        sub: "Everyone across your meetings — when you last spoke, and what is still open.",
        dur: 2900,
      });
      await film.showCursor(true);
      await film.wait(1200);
      await film.focus({ x: 300, y: 150, width: 1264, height: 200 }, { scale: 1.38, ms: 1200 });
      await film.cursorTo('text=Sarah Chen', { ms: 700 });
      await film.wait(450);
      await film.cursorTo('text=Marcus Reid', { ms: 620 });
      await film.wait(400);
      await film.cursorTo('text=Priya Nair', { ms: 700 });
      await film.wait(400);
      await film.showCursor(false);
      await film.wide({ ms: 850 });
      await film.wait(300);
    },
  },

  /* 9. OWNERSHIP — the closing argument, and the one the hook promised.
   *
   * A sealed Workspace discloses its NAME and nothing else: no note, no
   * transcript, no audio, not even to the app's own read paths. That is the
   * whole point of the beat, so frame the tree and the refusal together. */
  lock: {
    transition: "whip",
    async run(page, film) {
      await film.goto("/container/f-personal");
      await film.settle(800);
      await film.assertClean("lock");

      film.frame("title", { ms: 1100 });
      film.title("A door *only you*|hold the key to.", {
        kicker: "Lock a workspace",
        sub: "Notes, transcripts and audio seal behind Touch ID. Locked means locked — even to Murmur.",
        dur: 3600,
        kind: "title",
      });
      await film.wait(3980);

      // The sealed workspace discloses its NAME and nothing else, which is the
      // whole claim — so end on the padlock and the refusal in one frame. The
      // cursor glide is also what keeps this static route repainting.
      film.frame("lower", { ms: 1000 });
      await film.showCursor(true);
      film.title("And it is all *plain Markdown*.", {
        kicker: "Your files",
        sub: "In your own Obsidian vault. Delete Murmur tomorrow and every note is still yours.",
        dur: 2800,
      });
      await film.wait(1300);
      // Aim at the SIDEBAR, not the middle of the pane. A sealed workspace's
      // whole visible evidence is the red padlock beside its name plus the
      // refusal text — the rest of the route is deliberately empty, so a shot
      // centred on the content area photographs nothing at all.
      await film.focus({ x: 30, y: 240, width: 560, height: 380 }, { scale: 1.5, ms: 1300 });
      await film.cursorTo('mur-sidebar :text-is("Personal")', { ms: 800 });
      await film.wait(2000);
      await film.showCursor(false);
      await film.wide({ ms: 850 });
      await film.wait(350);
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
