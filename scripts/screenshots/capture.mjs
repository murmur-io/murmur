/*
 * Murmur README screenshot driver.
 *
 * Renders the REAL shipping Angular UI (ng serve on :1420) over a mocked Tauri
 * IPC layer + a privacy-safe demo world (./mock-tauri.js), then captures each
 * screen at 2× retina in the dark theme. No real vault / DB / mic / network is
 * touched — see ./README.md.
 *
 * Usage:
 *   NODE_PATH=<npx-playwright-cache>/node_modules node scripts/screenshots/capture.mjs [name...]
 * (the wrapper ./run.sh resolves NODE_PATH for you). Pass shot names to capture
 * a subset, e.g. `... capture.mjs record-brain brain`.
 */
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

// Playwright is not a package.json dep — resolve it from the path the wrapper
// (run.sh) discovered in the npx cache. NODE_PATH doesn't apply to ESM imports,
// so require() it explicitly.
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PATH || "playwright");

const __dirname = dirname(fileURLToPath(import.meta.url));
const OUT = join(__dirname, "..", "..", "docs", "screenshots");
const MOCK = readFileSync(join(__dirname, "mock-tauri.js"), "utf8");
const BASE = process.env.MURMUR_URL || "http://localhost:1420";

const EVENT_STATUS = "meetnotes://status";
const EVENT_LIVE_CAPTION = "murmur://live-caption";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A viewport wide enough to show the sidebar + content comfortably.
const APP = { width: 1440, height: 900 };

async function settle(page, ms = 700) {
  await page.evaluate(() => document.fonts && document.fonts.ready).catch(() => {});
  await sleep(ms);
}

/** Start a live recording in the mocked backend (sets stage + meetingId). */
async function startRecording(page, meetingId = "m-atlas-roadmap") {
  await page.evaluate(
    ([evt, id]) =>
      window.__demoEmit(evt, { stage: "recording", message: "Recording…", meetingId: id }),
    [EVENT_STATUS, meetingId],
  );
}

async function liveCaption(page, text) {
  await page.evaluate(
    ([evt, t]) => window.__demoEmit(evt, { text: t }),
    [EVENT_LIVE_CAPTION, text],
  );
}

/** Scroll the first heading/element matching `re` to the top of the scroller. */
async function scrollToText(page, re, block = "start") {
  await page.evaluate(
    ({ source, flags, block }) => {
      const rx = new RegExp(source, flags);
      const els = [...document.querySelectorAll("h1,h2,h3,h4,.section-head,.brain-models-label")];
      const el = els.find((e) => rx.test((e.textContent || "").trim()));
      if (el) el.scrollIntoView({ block, behavior: "instant" });
    },
    { source: re.source, flags: re.flags, block },
  );
}

// ── Shot registry ───────────────────────────────────────────────────────────
const SHOTS = {
  // The flagship: recording + an open @brain thread answered with citations.
  "record-brain": {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/record`, { waitUntil: "networkidle" });
      await page.waitForSelector("app-meeting-conversation", { timeout: 15000 });
      await startRecording(page);
      await liveCaption(page, "…and Marcus will own the Windows loopback spike so we're not blocked next sprint.");
      await settle(page, 500);
      const ta = page.locator("textarea.composer-input");
      await ta.fill("@brain what did we decide about the mobile redesign last quarter?");
      await ta.press("Enter");
      // Wait for the agent bubble to resolve (its answer text lands).
      await page.getByText(/removed that dependency/i).first().waitFor({ timeout: 8000 }).catch(() => {});
      await settle(page, 900);
    },
  },

  // Capture & transcribe hero — recording strip + live caption + notes flow.
  "hero-record": {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/record`, { waitUntil: "networkidle" });
      await page.waitForSelector("app-meeting-conversation", { timeout: 15000 });
      await startRecording(page);
      await liveCaption(page, "So the loopback work is unblocked — I can commit to a May 30 GA if nothing else moves.");
      await settle(page, 1200);
    },
  },

  // Ask across the whole vault — grounded answer + source chips.
  ask: {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/ask`, { waitUntil: "networkidle" });
      const input = page.locator("textarea, input[type=text]").first();
      await input.waitFor({ timeout: 15000 });
      await input.fill("What did we decide about Project Atlas and the mobile redesign this quarter?");
      await input.press("Enter");
      await page.getByText(/recurring throughline/i).first().waitFor({ timeout: 8000 }).catch(() => {});
      await settle(page, 900);
    },
  },

  // People & Projects knowledge graph.
  graph: {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/graph`, { waitUntil: "networkidle" });
      await page.waitForSelector("svg, canvas", { timeout: 15000 }).catch(() => {});
      await settle(page, 1400);
    },
  },

  // The /brain page — source cards + status header + graph.
  brain: {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/brain`, { waitUntil: "networkidle" });
      await settle(page, 1500);
    },
  },

  // Structured AI note (the meeting page is one long scroll → jump to "Analysis").
  "detail-note": {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/meeting/m-atlas-roadmap`, { waitUntil: "networkidle" });
      await settle(page, 600);
      await scrollToText(page, /^Analysis$/i);
      await settle(page, 800);
    },
  },

  // Me / Others transcript — lead with the segments.
  transcript: {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/meeting/m-atlas-roadmap`, { waitUntil: "networkidle" });
      await settle(page, 600);
      await scrollToText(page, /^Transcript$/i);
      await settle(page, 800);
    },
  },

  // Speaker + topic timeline (near the top of the meeting page).
  "detail-timeline": {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/meeting/m-atlas-roadmap`, { waitUntil: "networkidle" });
      await settle(page, 1000);
    },
  },

  // Library — folders + lock-aware rows.
  library: {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/library`, { waitUntil: "networkidle" });
      await settle(page, 1200);
    },
  },

  // Analytics dashboard.
  analytics: {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/analytics`, { waitUntil: "networkidle" });
      await settle(page, 1200);
    },
  },

  // Settings — Privacy section.
  "settings-privacy": {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/settings`, { waitUntil: "networkidle" });
      await settle(page, 500);
      await page.getByText(/^Privacy( & integrations)?$/i).first().click().catch(() => {});
      await settle(page, 900);
    },
  },

  // Settings — AI & Models (the provider seam: on-device + redacted-first cloud).
  settings: {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/settings`, { waitUntil: "networkidle" });
      await settle(page, 500);
      await page.getByText(/AI (&|and) Models/i).first().click().catch(() => {});
      await settle(page, 900);
    },
  },

  // Settings — the on-device brain model registry (Bielik / Qwen download cards).
  // A local Live role reveals the shared "Local models" block.
  "settings-brain": {
    viewport: APP,
    config: { brainBackend: "local", roleLiveConnection: "local", roleAskConnection: "local", brainModelId: "bielik-11b" },
    async run(page) {
      await page.goto(`${BASE}/settings`, { waitUntil: "networkidle" });
      await settle(page, 500);
      await page.getByText(/AI (&|and) Models/i).first().click().catch(() => {});
      await settle(page, 700);
      await scrollToText(page, /Local models/i);
      await settle(page, 800);
    },
  },

  // Onboarding wizard.
  onboarding: {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/onboarding`, { waitUntil: "networkidle" });
      await settle(page, 1200);
    },
  },

  // Notes editor — a body selection with the Brain command-menu popover open,
  // expanded to the grouped/several-actions view (the "More actions" state).
  "notes-editor-brain-menu": {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/notes/n-atlas-prd`, { waitUntil: "networkidle" });
      await page.waitForSelector("textarea.body-area", { timeout: 15000 });
      const body = page.locator("textarea.body-area");
      // Select a real chunk of NOTE_DOC_MD via setSelectionRange + a `select`
      // event (onBodySelect fires on mouseup/keyup/select — a native drag-select
      // over a wrapped textarea isn't reliable in Playwright).
      await body.evaluate((el) => {
        const text = el.value;
        const start = text.indexOf("Project Atlas");
        const end = start + 120;
        el.focus();
        el.setSelectionRange(start, end);
        el.dispatchEvent(new Event("select", { bubbles: true }));
      });
      await settle(page, 300);
      await page.locator("button.sel-ai").click();
      await page.waitForSelector(".brain-pop", { timeout: 5000 });
      await settle(page, 300);
      await page.locator(".pop-row-more").click();
      await settle(page, 500);
    },
  },

  // Notes-home — the "Shared brains" rail with an org selected, showing that
  // org's synced items (org-badge cards).
  "shared-brain-rail": {
    viewport: APP,
    async run(page) {
      await page.goto(`${BASE}/notes`, { waitUntil: "networkidle" });
      await page.waitForSelector(".org-row", { timeout: 15000 });
      await page.locator(".org-row").first().click();
      await settle(page, 900);
    },
  },

  // The floating always-on-top recorder bar (chromeless). The bar window is
  // transparent (it floats over the desktop) — give it a dark backdrop so the
  // frosted pill reads as it does over a dark desktop.
  bar: {
    viewport: { width: 480, height: 150 },
    async run(page) {
      await page.goto(`${BASE}/bar`, { waitUntil: "networkidle" });
      await startRecording(page);
      await settle(page, 700);
      // The bar component forces html/body background transparent, so paint a
      // dark desktop backdrop as a fixed child div (untouched by the component)
      // for the frosted pill to read against.
      await page.evaluate(() => {
        const bg = document.createElement("div");
        bg.style.cssText =
          "position:fixed;inset:0;z-index:-1;background:radial-gradient(120% 140% at 50% 0%, #232744 0%, #0a0a12 72%)";
        document.body.prepend(bg);
      });
      await settle(page, 400);
    },
  },
};

async function main() {
  const wanted = process.argv.slice(2);
  const names = wanted.length ? wanted : Object.keys(SHOTS);
  const browser = await chromium.launch();
  let ok = 0;
  for (const name of names) {
    const shot = SHOTS[name];
    if (!shot) {
      console.error(`✗ unknown shot: ${name}`);
      continue;
    }
    const ctx = await browser.newContext({
      viewport: shot.viewport,
      deviceScaleFactor: 2,
      colorScheme: "dark",
      locale: "en-US",
    });
    const page = await ctx.newPage();
    const errs = [];
    page.on("console", (m) => {
      if (m.type() === "error") errs.push(m.text());
    });
    await page.addInitScript(MOCK);
    if (shot.config) {
      await page.addInitScript(`window.__demoConfig = ${JSON.stringify(shot.config)};`);
    }
    try {
      await shot.run(page);
      const out = join(OUT, `${name}.png`);
      await page.screenshot({ path: out });
      console.log(`✓ ${name} → ${out}${errs.length ? `  (console errors: ${errs.length})` : ""}`);
      ok++;
    } catch (e) {
      console.error(`✗ ${name}: ${e.message}`);
    } finally {
      await ctx.close();
    }
  }
  await browser.close();
  console.log(`\n${ok}/${names.length} shots captured → ${OUT}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
