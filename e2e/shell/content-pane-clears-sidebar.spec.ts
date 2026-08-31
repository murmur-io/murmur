import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * A routed view that takes itself OUT of flow — `position: fixed`, to escape
 * `.app-main`'s padding and max-width — cannot read the content pane's left
 * edge from the box model, so it reads it from `--shell-content-inset`.
 *
 * REGRESSION this exists for: `/settings` hardcoded the offset instead
 * (`--space-2 + --space-8 + --space-2 + --space-2` = 88px, the OLD 64px icon
 * rail). When the rail became a 256px sidebar the settings section list
 * rendered UNDERNEATH the sidebar. Nothing caught it, because the rows stayed
 * VISIBLE — `toBeVisible()` passed — while `.sb-scroll` sat on top of them and
 * swallowed every click. Roughly thirty settings specs timed out on a click
 * rather than failing an assertion, which reads as a harness problem rather
 * than a layout bug.
 *
 * So this asserts the two things geometry alone does not: the pane starts at or
 * after the sidebar's right edge, AND a hit-test at a nav row's centre actually
 * lands inside the settings pane. It runs in BOTH sidebar states, because the
 * inset has two values and only one of them was ever wrong.
 */
async function insetProbe(page: Page) {
  const sidebar = page.getByRole("navigation", { name: "Primary navigation" });
  const pane = page.locator("app-settings");
  const [sidebarBox, paneBox] = await Promise.all([
    sidebar.boundingBox(),
    pane.boundingBox(),
  ]);
  expect(sidebarBox).not.toBeNull();
  expect(paneBox).not.toBeNull();
  return {
    sidebarRight: sidebarBox!.x + sidebarBox!.width,
    paneLeft: paneBox!.x,
  };
}

/** Whether a point at the row's centre reaches the settings pane, or is stolen. */
async function rowIsHittable(page: Page): Promise<boolean> {
  const row = page.getByText("AI & Models").first();
  await expect(row).toBeVisible();
  return row.evaluate((element) => {
    const box = element.getBoundingClientRect();
    const hit = document.elementFromPoint(
      box.x + box.width / 2,
      box.y + box.height / 2,
    );
    return !!hit?.closest("app-settings");
  });
}

test("the fixed settings pane clears the sidebar, expanded and collapsed", async ({
  page,
}) => {
  await mockTauri(page);
  await page.goto("/settings");

  const expanded = await insetProbe(page);
  expect(expanded.paneLeft).toBeGreaterThanOrEqual(expanded.sidebarRight);
  expect(await rowIsHittable(page)).toBe(true);

  await page
    .locator(".primary-sidebar .sb-top")
    .getByRole("button", { name: "Collapse sidebar" })
    .click();
  // The sidebar ANIMATES its width, so wait for the pane to follow it in rather
  // than reading a mid-transition box.
  await expect
    .poll(async () => Math.round((await insetProbe(page)).paneLeft))
    .toBeLessThan(Math.round(expanded.paneLeft));

  const collapsed = await insetProbe(page);
  expect(collapsed.paneLeft).toBeGreaterThanOrEqual(collapsed.sidebarRight);
  expect(await rowIsHittable(page)).toBe(true);

  // And the section really is reachable: this is the click that used to time out.
  await page.getByText("AI & Models").first().click();
  await expect(page.getByText("Where your AI runs")).toBeVisible();
});
