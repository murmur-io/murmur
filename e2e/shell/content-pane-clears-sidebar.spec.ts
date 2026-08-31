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

/**
 * Poll the invariant itself until it holds: the pane's left edge must sit at or
 * after the sidebar's right edge.
 *
 * Sampling the sidebar's width until two reads agree is NOT sufficient, and CI
 * proved it — the test went from hard-failing to merely flaky. The pane's `left`
 * comes from `--shell-content-inset` and moves the instant the
 * `sidebar-collapsed` class does, with no transition; the sidebar's width
 * ANIMATES. On a loaded runner the first animation frame can land more than a
 * sampling interval after the class flip, so two equal early reads look
 * "settled" while the panel is still at its old width.
 *
 * Polling the invariant is not vacuous: with the old hardcoded
 * `left: 88px` the pane starts at 88 against a sidebar reaching 264 (expanded)
 * or 108 (collapsed), so the gap stays negative and the poll times out RED.
 */
async function expectPaneClearsSidebar(page: Page): Promise<void> {
  await expect
    .poll(async () => {
      const { sidebarRight, paneLeft } = await insetProbe(page);
      return Math.round(paneLeft - sidebarRight);
    })
    .toBeGreaterThanOrEqual(0);
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

  await expectPaneClearsSidebar(page);
  const expanded = await insetProbe(page);
  expect(await rowIsHittable(page)).toBe(true);

  await page
    .locator(".primary-sidebar .sb-top")
    .getByRole("button", { name: "Collapse sidebar" })
    .click();
  await expect(page.locator("app-shell")).toHaveClass(/sidebar-collapsed/);

  // The inset must TRACK the collapse — proving it is read from the sidebar's
  // real width rather than being a second literal that happens to clear it.
  await expect
    .poll(async () => Math.round((await insetProbe(page)).paneLeft))
    .toBeLessThan(Math.round(expanded.paneLeft));
  await expectPaneClearsSidebar(page);
  expect(await rowIsHittable(page)).toBe(true);

  // And the section really is reachable: this is the click that used to time out.
  await page.getByText("AI & Models").first().click();
  await expect(page.getByText("Where your AI runs")).toBeVisible();
});
