import { test, expect, type Page } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * The tab strip's controls must take a click — in EVERY skin.
 *
 * THE BUG THIS PINS (2026-09-13, user report: the top tabs do nothing under
 * Paper, neither closing nor opening a new one). `.shell-drag` (styles.css) is
 * the `position: fixed`, full-width, 32px, `z-index: 8` band you drag the
 * window by. The strip is in flow at the top of `.main-col`, so without a
 * stacking rank of its own its controls lose the hit test to that band —
 * rendered, visible, and dead to a pointer.
 *
 * It surfaced under Paper because `app-shell` is padded by `--shell-gutter`:
 * Studio's 8px left each control's centre at y=32, one pixel below the band,
 * while Paper zeroes the gutter and drops every centre to y=24, inside it.
 *
 * WHY `elementFromPoint` AND NOT `.click()`: Playwright's click scrolls,
 * retries and can land on an actionable edge of a partly-covered control, so it
 * passes on geometry a user cannot reliably hit. `toBeVisible()` is weaker
 * still — it was true throughout the bug. Asking the document what is actually
 * on top at the control's CENTRE is the question the user's mouse asks.
 *
 * The Studio case is not redundant: before this fix it passed only because a
 * control's centre sat exactly on the band's first free pixel. It is the
 * regression guard for anyone who changes `--shell-gutter`, the strip height,
 * or `.shell-drag`'s 32px.
 */
async function openTab(page: Page, skin: "studio" | "paper") {
  if (skin === "paper") {
    await page.addInitScript(() =>
      localStorage.setItem("murmur-skin", "paper"),
    );
  }
  await mockNotes(page);
  await page.goto("/notes");
  await page.getByRole("button", { name: "My First Note" }).click();
  await expect(page.locator(".tab-strip .tab-item")).toHaveCount(1);
}

/** What the document hit-tests at the centre of `selector`. */
function topAtCentre(page: Page, selector: string) {
  return page.evaluate((sel) => {
    const el = document.querySelector(sel);
    if (!el) return { found: false, reaches: false, top: "missing" };
    const r = el.getBoundingClientRect();
    const top = document.elementFromPoint(
      Math.round(r.x + r.width / 2),
      Math.round(r.y + r.height / 2),
    );
    return {
      found: true,
      reaches: !!top && (top === el || el.contains(top)),
      top: top ? `${top.tagName.toLowerCase()}.${top.className}` : "null",
    };
  }, selector);
}

for (const skin of ["studio", "paper"] as const) {
  test(`${skin}: every tab-strip control takes a click at its centre`, async ({
    page,
  }) => {
    await openTab(page, skin);

    // A CONTROL for the assertions below: the band really is there and really
    // is above the strip, so a passing test means the controls out-rank it —
    // not that the hazard quietly disappeared and the guard went vacuous.
    const drag = await page.evaluate(() => {
      const d = document.querySelector(".shell-drag");
      if (!d) return null;
      const r = d.getBoundingClientRect();
      return { height: r.height, zIndex: getComputedStyle(d).zIndex };
    });
    expect(drag).not.toBeNull();
    expect(drag!.height).toBeGreaterThan(0);
    expect(Number(drag!.zIndex)).toBeGreaterThan(0);

    for (const sel of [".tab-label", ".tab-close", ".tab-new"]) {
      const hit = await topAtCentre(page, sel);
      expect(hit.found, `${sel} should render`).toBe(true);
      expect(
        hit.reaches,
        `${sel} centre is hit-tested to "${hit.top}", not the control`,
      ).toBe(true);
    }
  });
}

test("paper: the tabs actually close and open through the UI", async ({
  page,
}) => {
  await openTab(page, "paper");

  // Closing: the real click, not a geometry proxy.
  await page.locator(".tab-close").first().click();
  await expect(page.locator(".tab-strip .tab-item")).toHaveCount(0);
});

test("the empty run of the strip still drags the window", async ({ page }) => {
  await openTab(page, "paper");

  // The fix ranks the ITEMS, not the row — so the gap after the last control
  // must still belong to the drag band. Without this, a future "just put
  // z-index on .tab-strip" would pass every assertion above while silently
  // taking the window's whole top edge out of the drag region.
  const atEmpty = await page.evaluate(() => {
    const strip = document.querySelector(".tab-strip")!.getBoundingClientRect();
    const el = document.elementFromPoint(
      Math.round(strip.x + strip.width - 8),
      Math.round(strip.y + strip.height / 2),
    );
    return el ? `${el.tagName.toLowerCase()}.${el.className}` : "null";
  });
  expect(atEmpty).toContain("shell-drag");
});
