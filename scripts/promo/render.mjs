/*
 * Murmur promo-video renderer — STAGE 2 driver (see ./README.md).
 *
 * Opens ./compose.html over the footage recorded by ./record.mjs and screenshots
 * it one frame at a time: `await page.evaluate(t => seek(t))` then capture. The
 * page resolves `seek` only once every image for that instant has decoded, so a
 * slow machine renders the same film as a fast one — it just takes longer. That
 * is the property a real-time capture cannot give you, and the reason this is a
 * render rather than a recording.
 *
 * Usage:
 *   PLAYWRIGHT_PATH=… node scripts/promo/render.mjs [--fps 60] [--out DIR]
 *                                                   [--no-captions] [--no-endcard]
 *                                                   [--scenes a,b] [--to 12000]
 */
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync, mkdirSync, rmSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PATH || "playwright");

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "..", "..");
const PROMO = process.env.PROMO_DIR || join(ROOT, ".promo");

const W = 1920;
const H = 1080;

function parseArgs(argv) {
  const o = { fps: 60, out: join(PROMO, "render"), captions: true, endcard: true, scenes: null, to: null, from: 0, recycle: 400 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--fps") o.fps = Number(argv[++i]);
    else if (a === "--out") o.out = argv[++i];
    else if (a === "--no-captions") o.captions = false;
    else if (a === "--no-endcard") o.endcard = false;
    else if (a === "--scenes") o.scenes = argv[++i].split(",").map((s) => s.trim()).filter(Boolean);
    else if (a === "--to") o.to = Number(argv[++i]);
    else if (a === "--from") o.from = Number(argv[++i]);
    else if (a === "--recycle") o.recycle = Number(argv[++i]);
    else throw new Error(`unknown flag: ${a}`);
  }
  return o;
}

async function main() {
  const opt = parseArgs(process.argv.slice(2));
  const manifest = JSON.parse(readFileSync(join(PROMO, "manifest.json"), "utf8"));

  // Resuming keeps whatever is already on disk; a fresh run starts clean.
  if (!opt.from) rmSync(opt.out, { recursive: true, force: true });
  mkdirSync(opt.out, { recursive: true });

  const browser = await chromium.launch();
  const errors = [];

  const initOptions = JSON.stringify({
    captions: opt.captions,
    endcard: opt.endcard,
    scenes: opt.scenes,
    // file:// relative to compose.html, which lives beside the frames' parent.
    framesBase: `${PROMO.replace(/\/$/, "")}/frames/`.replace(/^/, "file://"),
  });

  /*
   * A FRESH browser context every `--recycle` frames.
   *
   * MEASURED, and not optional at 2×: the compositor walks thousands of distinct
   * 3200×1800 JPEGs, and Chromium keeps their DECODED bitmaps (~23 MB each)
   * alive in its image cache. The renderer starts at ~10 frames/s and decays —
   * this run was down to 0.6 frames/s with the browser holding 2.6 GB RSS, on a
   * machine with 10 GB still free, so it is cache thrash rather than an OOM.
   * Tearing the context down is the one thing that reliably drops those bitmaps.
   */
  async function openPage() {
    const ctx = await browser.newContext({
      viewport: { width: W, height: H },
      deviceScaleFactor: 1,
      colorScheme: "dark",
    });
    const page = await ctx.newPage();
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("console", (m) => {
      if (m.type() === "error") errors.push(m.text());
    });
    await page.addInitScript(`window.__promoManifest = ${JSON.stringify(manifest)};`);
    await page.addInitScript(`window.__promoOptions = ${initOptions};`);
    await page.goto(`file://${join(__dirname, "compose.html")}`, { waitUntil: "load" });
    if (errors.length) throw new Error(`compose.html failed: ${errors[0]}`);
    return { ctx, page };
  }

  let { ctx, page } = await openPage();

  /*
   * The product's typeface is loaded by relative file:// path. If that ever
   * breaks, every caption silently falls back to the system sans and the film
   * looks generic — a failure that is invisible in a log and obvious on a
   * landing page, so assert it rather than eyeballing it later.
   */
  const fontOk = await page.evaluate(async () => {
    await document.fonts.ready;
    return document.fonts.check('600 68px "Hanken Grotesk"');
  });
  if (!fontOk) throw new Error("the product typeface did not load — captions would render in a fallback font");

  const duration = await page.evaluate(() => window.__promoDuration);
  const end = opt.to != null ? Math.min(opt.to, duration) : duration;
  const step = 1000 / opt.fps;
  const total = Math.ceil(end / step);

  console.log(
    `▸ rendering ${total - opt.from} frames @ ${opt.fps}fps (${(end / 1000).toFixed(1)}s)` +
      `${opt.from ? `, resuming at ${opt.from}` : ""} → ${opt.out}`,
  );

  const t0 = Date.now();
  const writes = [];
  let sinceRecycle = 0;
  for (let i = opt.from; i < total; i++) {
    if (opt.recycle && sinceRecycle >= opt.recycle) {
      await Promise.all(writes.splice(0));
      await ctx.close();
      ({ ctx, page } = await openPage());
      sinceRecycle = 0;
    }
    sinceRecycle++;
    const t = i * step;
    await page.evaluate((ms) => window.seek(ms), t);
    const buf = await page.screenshot({ type: "png" });
    writes.push(writeFile(join(opt.out, `${String(i).padStart(6, "0")}.png`), buf));
    // Back-pressure: keep the write queue short so a long render does not hold
    // every frame in memory at once.
    if (writes.length >= 24) {
      await Promise.all(writes.splice(0));
    }
    if (i % 60 === 0 || i === total - 1) {
      const done = i - opt.from + 1;
      const pct = (((i + 1) / total) * 100).toFixed(0);
      const el = (Date.now() - t0) / 1000;
      const eta = (el / done) * (total - i - 1);
      process.stdout.write(`\r  ${pct}%  ${i + 1}/${total}  ${el.toFixed(0)}s elapsed, ~${eta.toFixed(0)}s left   `);
    }
  }
  await Promise.all(writes);
  process.stdout.write("\n");

  await ctx.close();
  await browser.close();

  if (errors.length) {
    console.error(`⚠ ${errors.length} page errors during render; first: ${errors[0]}`);
  }
  console.log(`✓ ${total} frames → ${opt.out}`);
}

main().catch((e) => {
  console.error(`\n✗ ${e.message}`);
  process.exit(1);
});
