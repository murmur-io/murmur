/*
 * Murmur promo-video recorder — STAGE 1 of scripts/promo (see ./README.md).
 *
 * Drives the REAL shipping Angular UI over the screenshot harness's mocked Tauri
 * IPC + privacy-safe demo world (../screenshots/mock-tauri.js), and captures raw
 * footage as a numbered JPEG sequence plus a timeline of *authored* camera and
 * caption marks.
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
 * 16:9 at 1600×900 CSS px. The delivered film is 1920×1080 with the app inside a
 * 1664×936 window, so at rest one CSS pixel of UI is ~1.04 delivered pixels —
 * the app reads at its natural size — while the captured pixels behind it are 2×,
 * which is the headroom the push-ins spend.
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
 * to guess when to push the camera in or when a caption should land: the edit is
 * a by-product of performing the scene.
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
     * A caption. `dur` is how long it stays up; stage 2 gives it a rise-and-fade
     * in and out, so the authored time is when it should be READ, not when the
     * animation starts.
     */
    title(text, { sub = "", dur = 2800, kind = "lower" } = {}) {
      scene.captions.push({ t: at(), text, sub, dur, kind });
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
// Beat order follows the brief: hook → problem → solution → proof → ownership →
// CTA. Each scene is one continuous take; stage 2 cross-dissolves between them.
const SCENES = {
  /* 1. HOOK — open on the product mid-action. No logo, no title card. */
  record: {
    async run(page, film) {
      await film.goto("/record");
      await film.settle(900);
      await film.startRecording();
      await film.wait(700);
      await film.assertClean("record");

      // Beat 1 — the claim, over the WIDE shot. At rest the window sits above
      // the caption band, so the hero title has the frame to itself.
      film.title("Nothing leaves your Mac.", {
        sub: "Recording and transcription run on device.",
        dur: 2900,
        kind: "hero",
      });
      await film.wait(3600);

      // Beat 2 — push in on the app's own live-transcript bar and let two lines
      // arrive. No title here: the app is doing the talking.
      await film.liveCaption("…so the sync-layer dependency is gone — Atlas is unblocked for GA.");
      await film.focus({ x: 120, y: 800, width: 1360, height: 120 }, { scale: 1.4, ms: 1500 });
      await film.wait(2100);
      await film.liveCaption("Then let's commit to the 14th. Priya, you own the migration note.");
      await film.wait(2300);

      // Beat 3 — pull back out, and land the problem the product exists for.
      await film.wide({ ms: 1200 });
      await film.wait(500);
      film.title("Meetings end. The decisions scatter.", { dur: 2500 });
      await film.wait(3000);
    },
  },

  /* 2. SOLUTION — the dual-stream transcript, the thing that is actually hard. */
  transcript: {
    async run(page, film) {
      await film.goto("/meeting/m-atlas-roadmap");
      await film.settle(800);
      await film.showCursor(true);
      film.title("Transcribed on-device — and split by speaker.", {
        sub: "whisper.cpp on Metal. Your voice and theirs, on separate tracks.",
        dur: 3400,
      });
      await film.click("button:has-text('Audio')", { after: 900 });
      await film.assertClean("transcript");
      // The Me/Others columns live in the upper-middle of the content column;
      // bias up so the caption band stays clear of the rows being read.
      await film.focus({ x: 380, y: 300, width: 900, height: 380 }, { scale: 1.62, ms: 1400 });
      await film.wait(2800);
      await film.wide({ ms: 1000 });
      await film.wait(900);
    },
  },

  /* 3. PROOF — ask across everything, with citations. */
  ask: {
    async run(page, film) {
      await film.goto("/ask");
      await film.settle(700);
      await film.showCursor(true);
      film.title("Ask across everything you've recorded.", {
        sub: "Grounded in your own vault — never a guess.",
        dur: 3200,
      });
      const input = "textarea, input[type=text]";
      await film.type(input, "What did we decide about Atlas, and who owns what's left?", { cps: 17 });
      await film.wait(320);
      await page.keyboard.press("Enter");
      await film.wait(1800);
      await film.assertClean("ask");
      await film.focus({ x: 300, y: 220, width: 1000, height: 420 }, { scale: 1.5, ms: 1300 });
      await film.wait(2800);
      await film.wide({ ms: 900 });
      await film.wait(400);
      film.title("Every claim cites the second of audio it came from.", { dur: 2600 });
      await film.wait(3000);
    },
  },

  /* 4. PROOF — the brain as a map. Motion for free: the force layout settles.
   *
   * NOT `/graph`. That route is the entity BROWSER — a list of People and
   * Projects as chips — which photographs as a list and flatly fails to deliver
   * the caption's promise. The actual map lives behind the "Full brain graph"
   * disclosure on `/brain`, which is where the screenshot harness gets it too
   * (scripts/screenshots/capture.mjs, the `full-brain-graph` shot). */
  graph: {
    async run(page, film) {
      await film.goto("/brain");
      await film.settle(900);
      await film.showCursor(true);
      await film.click("button:has-text('Full brain graph')", { after: 700 });
      await page.waitForSelector("app-full-brain-graph", { timeout: 15_000 });
      await film.showCursor(false);
      // "Clusters" packs the map by community; "Layers" leaves most of the
      // canvas empty at this size. "Fit" then centres it.
      const clusters = page.locator("button", { hasText: /^\s*Clusters\s*$/ }).first();
      if (await clusters.count()) await clusters.click();
      await film.wait(1800);
      const fit = page.locator("button", { hasText: /^\s*Fit\s*$/ }).first();
      if (await fit.count()) await fit.click();
      // Park the CANVAS in frame, not the component host. The host starts at the
      // legend and control rows, so centring it leaves the map itself below the
      // fold — which is what the first take photographed. Scroll whichever
      // ancestor actually scrolls until the canvas is centred.
      await page.evaluate(() => {
        const c = document.querySelector("app-full-brain-graph canvas");
        if (!c) return;
        c.scrollIntoView({ block: "center", behavior: "instant" });
        const r = c.getBoundingClientRect();
        const delta = r.top + r.height / 2 - window.innerHeight / 2;
        if (Math.abs(delta) < 8) return;
        let n = c.parentElement;
        while (n && n !== document.body) {
          if (n.scrollHeight > n.clientHeight + 4) { n.scrollTop += delta; return; }
          n = n.parentElement;
        }
        window.scrollBy(0, delta);
      });
      await film.wait(900);
      await film.assertClean("graph");

      film.title("Meetings, notes, documents, people — one brain.", { dur: 3000 });
      // A slow, continuous push on the map. The force layout is still settling,
      // so this beat has motion of its own — the camera only frames it.
      await film.focus("app-full-brain-graph", { scale: 1.3, ms: 3400 });
      await film.wait(3600);
      await film.wide({ ms: 1200 });
      await film.wait(800);
    },
  },

  /* 5. OWNERSHIP — a sealed Space discloses its NAME and nothing else. */
  lock: {
    async run(page, film) {
      await film.goto("/container/f-personal");
      await film.settle(500);
      await film.showCursor(true);
      await film.click('nav.global-rail button[aria-label="Spaces"]', { after: 900 });
      await film.assertClean("lock");
      film.title("A door you hold the key to.", {
        sub: "Lock a Space and its notes, transcripts and audio seal behind Touch ID.",
        dur: 3400,
        kind: "hero",
      });
      // The Spaces panel is the left third; a sealed Space discloses its NAME and
      // nothing else, which is the whole point of the beat — frame the tree.
      await film.focus({ x: 60, y: 220, width: 620, height: 460 }, { scale: 1.7, ms: 1500 });
      await film.wait(3200);
      await film.wide({ ms: 1000 });
      await film.wait(900);
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
  const scene = { id: name, camera: [], captions: [], frames: [] };
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

  // The film clock starts at the scene's first frame, not at t0 — a page that
  // paints late would otherwise open on a hold of nothing.
  const skew = scene.frames.length ? scene.frames[0].t : 0;
  for (const f of scene.frames) f.t -= skew;
  for (const c of scene.camera) c.t = Math.max(0, c.t - skew);
  for (const c of scene.captions) c.t = Math.max(0, c.t - skew);
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

  rmSync(join(OUT, "frames"), { recursive: true, force: true });
  mkdirSync(join(OUT, "frames"), { recursive: true });

  const browser = await chromium.launch({ args: launchArgs, headless: HEADLESS });
  const scenes = [];
  try {
    for (const name of names) {
      const dir = join(OUT, "frames", name);
      process.stdout.write(`▸ ${name} … `);
      const scene = await recordScene(browser, name, dir);
      scenes.push(scene);
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

  const manifest = { version: VERSION, viewport: V, scenes };
  writeFileSync(join(OUT, "manifest.json"), JSON.stringify(manifest, null, 2));
  const total = scenes.reduce((a, s) => a + s.dur, 0) / 1000;
  console.log(`\n${scenes.length} scenes · ${total.toFixed(1)}s of footage → ${OUT}`);
}

main().catch((e) => {
  console.error(`\n✗ ${e.message}`);
  process.exit(1);
});
