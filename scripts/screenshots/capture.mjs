/*
 * Murmur product-screenshot driver.
 *
 * Renders the REAL shipping Angular UI (ng serve) over a mocked Tauri IPC layer
 * plus a privacy-safe demo world (./mock-tauri.js), then captures each screen at
 * 2× retina in the dark theme. No real vault / DB / mic / network is touched —
 * see ./README.md.
 *
 * Usage:
 *   PLAYWRIGHT_PATH=<npx-cache>/node_modules/playwright node scripts/screenshots/capture.mjs [name...]
 * (the wrapper ./run.sh resolves PLAYWRIGHT_PATH for you). Pass shot names to
 * capture a subset, e.g. `... capture.mjs dashboard tasks`.
 *
 * TWO GUARANTEES THIS FILE ENFORCES, because a marketing image is published and
 * cannot be un-published:
 *
 *   1. The version in the About shot comes from package.json (`__demoVersion`),
 *      never a literal. The mock shipped "0.6.3" into 2.0-era captures.
 *   2. Every shot is scanned for real-world data before it is written (see
 *      PRIVACY_DENY). A shot that trips the scan is REFUSED, not saved. "No
 *      sensitive data" is then a property of the tool, not a promise from
 *      whoever ran it.
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
const ROOT = join(__dirname, "..", "..");
const OUT = process.env.MURMUR_SHOT_DIR || join(ROOT, "docs", "screenshots");
const MOCK = readFileSync(join(__dirname, "mock-tauri.js"), "utf8");
const BASE = process.env.MURMUR_URL || "http://localhost:1420";
const VERSION = JSON.parse(readFileSync(join(ROOT, "package.json"), "utf8")).version;

const EVENT_STATUS = "meetnotes://status";
const EVENT_LIVE_CAPTION = "murmur://live-caption";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A viewport wide enough to show the rail, a context panel, and content.
const APP = { width: 1440, height: 900 };
const TALL = { width: 1440, height: 1000 };

/*
 * ── The privacy gate ───────────────────────────────────────────────────────
 * Patterns that must never reach a published screenshot. Two kinds:
 *
 *   - identity/machine leaks (a real name, email, or home directory), which
 *     would mean the mock was bypassed and a real vault rendered;
 *   - strings from the operator's actual vault that once leaked into this
 *     repo's e2e fixtures. They are fine in a test; they are not fine on a
 *     landing page.
 *
 * Plus a stale product name, because "MeetNotes" in a 2.x shot is its own kind
 * of untruth.
 */
const PRIVACY_DENY = [
  [/jakub|gawronski|unitedrepaircentre|kgm004a/i, "operator identity"],
  [/\/Users\/(?!demo\b)[a-z]/i, "a real home directory"],
  [/weronika|leszek|alcon|organizator|mówca/i, "operator vault content (leaked into e2e fixtures)"],
  [/MeetNotes/, "the pre-rename product name"],
];

// Any email in a shot must belong to the fictional demo world.
const EMAIL_RE = /[\w.+-]+@[\w.-]+/g;
const EMAIL_ALLOW = /@(sonora|example)(\.|$|\b)/i;

/**
 * Scan what the page actually renders. Returns a list of violations; empty means
 * the shot is publishable. Reads `innerText` (what a reader sees) plus the
 * document title (what the window chrome shows).
 */
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
  await page.evaluate(([evt, t]) => window.__demoEmit(evt, { text: t }), [EVENT_LIVE_CAPTION, text]);
}

/** Open one of the shell's two context panels ("Spaces" / "Browse"). */
async function openPanel(page, label) {
  const btn = page.locator(`nav.global-rail button[aria-label="${label}"]`);
  await btn.waitFor({ timeout: 10_000 });
  const pressed = await btn.getAttribute("aria-pressed");
  if (pressed !== "true") await btn.click();
  await settle(page, 500);
}

/**
 * Scroll the first heading/element matching `re` into view. `lift` scrolls back
 * up by that many pixels afterwards — `scrollIntoView({block:"start"})` alone
 * parks the target UNDER a sticky header, which in the Audio panel drew the
 * "Timeline" heading on top of the player.
 */
async function scrollToText(page, re, { block = "start", lift = 0 } = {}) {
  await page.evaluate(
    ({ source, flags, block, lift }) => {
      const rx = new RegExp(source, flags);
      const els = [
        ...document.querySelectorAll("h1,h2,h3,h4,.section-head,.brain-models-label,.card-title"),
      ];
      const el = els.find((e) => rx.test((e.textContent || "").trim()));
      if (!el) return;
      el.scrollIntoView({ block, behavior: "instant" });
      if (!lift) return;
      // Walk up to whichever ancestor actually scrolls and back it off.
      let node = el.parentElement;
      while (node && node !== document.body) {
        if (node.scrollHeight > node.clientHeight + 4) {
          node.scrollTop = Math.max(0, node.scrollTop - lift);
          return;
        }
        node = node.parentElement;
      }
      window.scrollBy(0, -lift);
    },
    { source: re.source, flags: re.flags, block, lift },
  );
}

/** Click a tab/segment/lens by its visible label, inside an optional scope. */
async function clickLabel(page, label, scope = "") {
  const loc = page.locator(`${scope} button`, { hasText: new RegExp(`^\\s*${label}\\s*$`) }).first();
  await loc.waitFor({ timeout: 10_000 });
  await loc.click();
  await settle(page, 500);
}

const goto = (page, path) => page.goto(`${BASE}${path}`, { waitUntil: "networkidle" });

// ── Shot registry ───────────────────────────────────────────────────────────
// Order is the reading order of the README, so a full run reads like the docs.
const SHOTS = {
  // ── The 2.0 shell: one tree, behind a rail and a context panel ──
  "hero-spaces": {
    viewport: APP,
    async run(page) {
      await goto(page, "/container/f-atlas");
      await openPanel(page, "Spaces");
      await settle(page, 900);
    },
  },

  // The lock model, on screen: a sealed Space discloses its NAME and nothing
  // else — no counts, no children, no items.
  "spaces-locked": {
    viewport: { width: 1440, height: 640 },
    async run(page) {
      await goto(page, "/container/f-personal");
      await openPanel(page, "Spaces");
      await settle(page, 900);
    },
  },

  // ── Capture ──
  "hero-record": {
    viewport: APP,
    async run(page) {
      await goto(page, "/record");
      await startRecording(page);
      await settle(page, 600);
      await liveCaption(page, "…so the sync-layer dependency is gone — Atlas is unblocked for GA.");
      await settle(page, 900);
    },
  },

  // Ask Brain DURING the meeting: the recording never stops, and the answer is
  // grounded in the vault with citations. `.ask-pill` summons the opaque panel;
  // the answer arrives through `ask_assistant_chat`.
  "record-brain": {
    viewport: APP,
    async run(page) {
      await goto(page, "/record");
      await startRecording(page);
      await settle(page, 900);
      await page.locator("button.ask-pill").click({ timeout: 10_000 });
      await page.waitForSelector("input.ask-input", { timeout: 10_000 });
      await page.locator("input.ask-input").fill("What did we promise Acme on the renewal call?");
      await settle(page, 250);
      await page.locator("button.send-btn").click();
      await settle(page, 1600);
    },
  },

  bar: {
    viewport: { width: 480, height: 150 },
    async run(page) {
      await goto(page, "/bar");
      await startRecording(page);
      await settle(page, 700);
      // The bar component forces html/body transparent (it floats over the
      // desktop), so paint a dark desktop backdrop for the frosted pill to read
      // against — as a fixed child the component never touches.
      await page.evaluate(() => {
        const bg = document.createElement("div");
        bg.style.cssText =
          "position:fixed;inset:0;z-index:-1;background:radial-gradient(120% 140% at 50% 0%, #232744 0%, #0a0a12 72%)";
        document.body.prepend(bg);
      });
      await settle(page, 400);
    },
  },

  // ── Boards ──
  dashboard: {
    viewport: { width: 1440, height: 860 },
    async run(page) {
      await goto(page, "/dashboards/d-atlas");
      await settle(page, 1200);
    },
  },

  "dashboard-commitments": {
    viewport: APP,
    async run(page) {
      await goto(page, "/dashboards/d-atlas");
      await settle(page, 900);
      await clickLabel(page, "Commitments", "nav.lenses");
    },
  },

  "dashboards-home": {
    viewport: APP,
    async run(page) {
      await goto(page, "/dashboards");
      await settle(page, 900);
    },
  },

  // ── The brain ──
  ask: {
    viewport: { width: 1440, height: 780 },
    async run(page) {
      await goto(page, "/ask");
      await settle(page, 600);
      const input = page.locator("textarea, input[type=text]").first();
      await input.fill("What did we decide about Atlas, and who owns what's left?");
      await settle(page, 200);
      await page.keyboard.press("Enter");
      await settle(page, 1600);
    },
  },

  brain: {
    viewport: APP,
    async run(page) {
      await goto(page, "/brain");
      await settle(page, 1000);
    },
  },

  graph: {
    viewport: { width: 1440, height: 760 },
    async run(page) {
      await goto(page, "/graph");
      await settle(page, 1600);
    },
  },

  // The whole brain as one map — meetings, notes, documents and people. Lives
  // behind a disclosure on /brain.
  "full-brain-graph": {
    viewport: { width: 1440, height: 1080 },
    async run(page) {
      await goto(page, "/brain");
      await settle(page, 900);
      await page.locator("button", { hasText: /Full brain graph/ }).first().click();
      await page.waitForSelector("app-full-brain-graph", { timeout: 10_000 });
      // The force layout needs a moment to settle before it is worth a picture.
      await settle(page, 3200);
      // "Clusters" packs the map by community; "Layers" stacks four sparse rows
      // and leaves most of the canvas empty at this size.
      const clusters = page.locator("button", { hasText: /^\s*Clusters\s*$/ }).first();
      if (await clusters.count()) {
        await clusters.click();
        await settle(page, 2600);
      }
      // "Fit" scales/centres the map in its canvas; without it the layered layout
      // sits squeezed against one edge.
      const fit = page.locator("button", { hasText: /^\s*Fit\s*$/ }).first();
      if (await fit.count()) {
        await fit.click();
        await settle(page, 900);
      }
      // Park the map itself in frame, not the disclosure row above it.
      await page.evaluate(() => {
        const el = document.querySelector("app-full-brain-graph");
        if (el) el.scrollIntoView({ block: "center", behavior: "instant" });
      });
      await settle(page, 900);
    },
  },

  people: {
    viewport: { width: 1440, height: 700 },
    async run(page) {
      await goto(page, "/people");
      await settle(page, 900);
    },
  },

  // ── One meeting ──
  "detail-note": {
    viewport: TALL,
    async run(page) {
      await goto(page, "/meeting/m-atlas-roadmap");
      await settle(page, 1200);
    },
  },

  // The transcript and the timeline both live under the AUDIO tab — the meeting
  // has exactly three tabs (Note / Audio / Share), and the transcript is loaded
  // lazily when that tab opens (it used to ship on every detail read: ~520 kB for
  // an hour of audio the Note tab never rendered).
  // Receipts: the "card receipts" block in the Note tab, one chip per grounded
  // claim, each a jump to the second of audio it came from.
  "detail-receipts": {
    viewport: { width: 1440, height: 820 },
    async run(page) {
      await goto(page, "/meeting/m-atlas-roadmap");
      await page.waitForSelector(".receipts", { timeout: 15_000 });
      await settle(page, 900);
      await page.evaluate(() => {
        const el = document.querySelector(".receipts");
        if (el) el.scrollIntoView({ block: "center", behavior: "instant" });
      });
      await settle(page, 600);
    },
  },

  transcript: {
    viewport: APP,
    async run(page) {
      await goto(page, "/meeting/m-atlas-roadmap");
      await settle(page, 700);
      await clickLabel(page, "Audio");
      await settle(page, 1000);
      await scrollToText(page, /transcript|me\b|others/i, { lift: 96 });
      await settle(page, 500);
    },
  },

  "detail-timeline": {
    viewport: APP,
    async run(page) {
      await goto(page, "/meeting/m-atlas-roadmap");
      await settle(page, 700);
      await clickLabel(page, "Audio");
      await settle(page, 1200);
      await scrollToText(page, /timeline|chapters|speakers/i, { lift: 178 });
      await settle(page, 700);
    },
  },

  // ── Notes ──
  "notes-home": {
    viewport: APP,
    async run(page) {
      await goto(page, "/notes");
      await settle(page, 1000);
    },
  },

  "notes-editor-brain-menu": {
    viewport: APP,
    async run(page) {
      await goto(page, "/notes/n-atlas-prd");
      await page.waitForSelector("textarea.body-area", { timeout: 15_000 });
      await settle(page, 900);
      // The editor body is a textarea; the selection bubble is raised by
      // `(mouseup)="onBodySelect()"`, so a programmatic range needs that event
      // fired by hand. Select one sentence, not the whole note — the popover
      // shows the selected text.
      await page.evaluate(() => {
        const el = document.querySelector("textarea.body-area");
        if (!el) return;
        const body = el.value;
        // A sentence in the middle of the note reads better than the H1.
        const start = Math.max(0, body.indexOf("p95"));
        const dot = body.indexOf(".", start);
        el.focus();
        el.setSelectionRange(start, dot > start ? dot + 1 : Math.min(body.length, start + 120));
        el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
      });
      await settle(page, 500);
      await page.locator("button.sel-ai").click({ timeout: 10_000 });
      await page.waitForSelector(".brain-pop", { timeout: 10_000 });
      await settle(page, 400);
      // Expand past the first few actions so the shot shows the real catalog.
      const more = page.locator(".pop-row-more");
      if (await more.count()) {
        await more.first().click();
        await settle(page, 500);
      }
    },
  },

  // ── Shared work ──
  // Tasks is a list + detail split; landing on it with nothing selected shows
  // only "Select a task", so open the first one.
  tasks: {
    viewport: APP,
    async run(page) {
      await goto(page, "/tasks");
      await settle(page, 1000);
      const row = page.locator(".task-row, li.task, [role=listitem]").first();
      if (await row.count()) {
        await row.click();
      } else {
        await page.locator("text=Acme — return contract redlines").first().click();
      }
      await settle(page, 1000);
    },
  },

  reminders: {
    viewport: APP,
    async run(page) {
      await goto(page, "/reminders");
      await settle(page, 1000);
    },
  },

  "shared-brains": {
    viewport: { width: 1440, height: 720 },
    async run(page) {
      await goto(page, "/shared-brains");
      await settle(page, 1000);
    },
  },

  // ── Browse & insight ──
  library: {
    viewport: APP,
    async run(page) {
      await goto(page, "/library");
      await openPanel(page, "Browse");
      await settle(page, 900);
    },
  },

  analytics: {
    viewport: APP,
    async run(page) {
      await goto(page, "/analytics");
      await settle(page, 1300);
    },
  },

  // ── Settings ──
  "settings-privacy": {
    viewport: TALL,
    async run(page) {
      await goto(page, "/settings?section=privacy");
      await settle(page, 1000);
    },
  },

  "settings-ai": {
    viewport: TALL,
    async run(page) {
      await goto(page, "/settings?section=ai");
      await settle(page, 1000);
    },
  },

  // The per-role engine/model registry lives behind "Customize models"; without
  // opening it this shot was a duplicate of settings-ai.
  "settings-models": {
    viewport: TALL,
    async run(page) {
      await goto(page, "/settings?section=ai");
      await settle(page, 900);
      // "Customize models" calls the store's expandAdvanced(), which reveals the
      // engine cards FURTHER DOWN the page — so click, then scroll to "Engines".
      const more = page.locator("button", { hasText: /Customize models/ }).first();
      if (await more.count()) {
        await more.click();
        await settle(page, 1000);
      }
      await scrollToText(page, /^Engines$/i, { lift: 60 });
      await settle(page, 700);
    },
  },

  "settings-imports": {
    viewport: APP,
    async run(page) {
      await goto(page, "/settings?section=imports");
      await settle(page, 1000);
    },
  },

  // The README/landing banner. NOT the app — a standalone page (./banner.html)
  // that reuses the app's tokens and its brand mark, so the header image cannot
  // drift from the product the way the hand-drawn one did.
  banner: {
    viewport: { width: 1280, height: 448 },
    async run(page) {
      await page.goto(`file://${join(__dirname, "banner.html")}`, { waitUntil: "load" });
      await settle(page, 800);
    },
  },

  onboarding: {
    viewport: { width: 1120, height: 700 },
    async run(page) {
      await goto(page, "/onboarding");
      await settle(page, 1000);
    },
  },
};

async function main() {
  const wanted = process.argv.slice(2);
  const names = wanted.length ? wanted : Object.keys(SHOTS);
  const browser = await chromium.launch();
  let ok = 0;
  const refused = [];
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
    // `__demoRich` opts the mock into the aggregate 2.0 lists (the Spaces tree,
    // boards, tasks, ask grounding). They are opt-in because this mock is also the
    // e2e suite's base fixture — see the SHARED FIXTURE WARNING in mock-tauri.js.
    await page.addInitScript("window.__demoRich = true;");
    await page.addInitScript(`window.__demoVersion = ${JSON.stringify(VERSION)};`);
    await page.addInitScript(MOCK);
    if (shot.config) {
      await page.addInitScript(`window.__demoConfig = ${JSON.stringify(shot.config)};`);
    }
    try {
      await shot.run(page);
      const leaks = await privacyViolations(page);
      if (leaks.length) {
        refused.push(name);
        console.error(`⛔ ${name}: REFUSED — ${leaks.join("; ")}`);
        continue;
      }
      const out = join(OUT, `${name}.png`);
      await page.screenshot({ path: out });
      console.log(`✓ ${name} → ${out}${errs.length ? `  (console errors: ${errs.length})` : ""}`);
      ok++;
    } catch (e) {
      console.error(`✗ ${name}: ${e.message.split("\n")[0]}`);
    } finally {
      await ctx.close();
    }
  }
  await browser.close();
  console.log(`\n${ok}/${names.length} shots captured → ${OUT}`);
  if (refused.length) {
    console.error(`REFUSED for privacy: ${refused.join(", ")}`);
    process.exitCode = 1;
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
