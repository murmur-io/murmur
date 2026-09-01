import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Settings is a MODAL over the whole window.
 *
 * It used to be a `position: fixed` pane that started after the shell sidebar,
 * reading that offset from `--shell-content-inset` — and this file (then
 * `e2e/shell/content-pane-clears-sidebar.spec.ts`) pinned the geometry: pane
 * left edge ≥ sidebar right edge, in both sidebar states. That invariant is
 * gone with the layout it described; the dialog now covers the sidebar on
 * purpose, and collapsing the sidebar from inside Settings is no longer a
 * thing you can do.
 *
 * What SURVIVES is the lesson the original regression taught, and it is the
 * half worth keeping: assert HIT-TESTING, not visibility. That bug —
 * `/settings` hardcoding an 88px offset against a 256px sidebar — left every
 * nav row perfectly visible while `.sb-scroll` sat on top of them and
 * swallowed the clicks, so `toBeVisible()` passed and about thirty settings
 * specs timed out on a click instead. A modal has exactly the same failure
 * mode available to it (a scrim at the wrong z-index, a stacking context that
 * traps the panel underneath the sidebar), so the hit-test assertions below
 * carry over unchanged in spirit.
 *
 * NOT covered here: `--shell-content-inset` itself, whose remaining consumer is
 * the filing-recovery banner. Nothing exercises that mechanism now — worth a
 * spec of its own rather than a claim from this one.
 */

/** Does a point at this element's centre actually reach the settings dialog? */
async function hitsSettings(page: Page, locator: ReturnType<Page["locator"]>) {
  await expect(locator).toBeVisible();
  return locator.evaluate((element) => {
    const box = element.getBoundingClientRect();
    const hit = document.elementFromPoint(
      box.x + box.width / 2,
      box.y + box.height / 2,
    );
    return !!hit?.closest("app-settings");
  });
}

test("the settings dialog covers the window and owns the clicks", async ({
  page,
}) => {
  await mockTauri(page);
  await page.goto("/settings");

  const pane = page.locator("app-settings");
  await expect(pane).toBeVisible();

  // It is the window, not a column beside the sidebar: the dialog starts at or
  // before the sidebar's left edge and ends after its right one. Stated against
  // the SIDEBAR rather than against exact viewport pixels — a scrollbar or a
  // fractional device ratio must not decide whether this passes.
  const sidebar = page.getByRole("navigation", { name: "Primary navigation" });
  const [paneBox, sidebarBox] = await Promise.all([
    pane.boundingBox(),
    sidebar.boundingBox(),
  ]);
  expect(paneBox).not.toBeNull();
  expect(sidebarBox).not.toBeNull();
  expect(paneBox!.x).toBeLessThanOrEqual(sidebarBox!.x);
  expect(paneBox!.x + paneBox!.width).toBeGreaterThanOrEqual(
    sidebarBox!.x + sidebarBox!.width,
  );

  // A nav row's centre reaches the dialog — the assertion the original
  // regression needed and visibility could not give.
  expect(await hitsSettings(page, page.getByText("AI & Models").first())).toBe(
    true,
  );

  // And the app BEHIND is inert: a point over the shell sidebar lands on the
  // dialog, not on the sidebar.
  expect(await hitsSettings(page, sidebar)).toBe(true);

  // The click that used to time out.
  await page.getByText("AI & Models").first().click();
  await expect(page.getByText("Where your AI runs")).toBeVisible();
});

test("Escape, the close button and the scrim all dismiss it", async ({
  page,
}) => {
  await mockTauri(page);

  await page.goto("/settings");
  await expect(page.locator("app-settings")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator("app-settings")).toHaveCount(0);

  await page.goto("/settings");
  await page.getByRole("button", { name: "Close settings" }).click();
  await expect(page.locator("app-settings")).toHaveCount(0);

  await page.goto("/settings");
  // Click the scrim's left margin, BELOW the top band: that band is the window
  // drag strip (the shell's own is behind the dialog), and a click there must
  // drag rather than dismiss.
  await page.locator(".settings-scrim").click({ position: { x: 4, y: 300 } });
  await expect(page.locator("app-settings")).toHaveCount(0);
});

test("Escape inside the search box clears it before it closes anything", async ({
  page,
}) => {
  await mockTauri(page);
  await page.goto("/settings");

  const search = page.getByRole("searchbox", { name: "Search settings" });
  await search.fill("obsidian");
  await search.press("Escape");

  await expect(search).toHaveValue("");
  await expect(page.locator("app-settings")).toBeVisible();
});
