import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Every icon in the meeting's command row explains itself on hover.
 *
 * The row is now almost entirely glyphs, and what it had was the native
 * `title`: roughly a second and a half of delay, the OS's styling, nothing at
 * all for a keyboard user. The report was simply "hovering an icon does not
 * tell me what it does", and that was accurate.
 *
 * The oracle that matters is the LAST one: a control carrying both `title` and
 * `[appTooltip]` shows two tooltips stacked on each other, ours at 350ms and
 * the OS's a second later. That is invisible to a test that only asserts the
 * bubble appears, which is why the native attribute is asserted absent rather
 * than assumed gone.
 */
async function openMeeting(page: Page): Promise<void> {
  await mockTauri(page);
  await page.goto("/meeting/m-atlas-roadmap");
  await expect(page.locator(".head-actions")).toBeVisible();
}

const bubble = ".mur-tooltip";

test("hovering an icon explains it, and leaving takes the explanation away", async ({
  page,
}) => {
  await openMeeting(page);
  const resummarize = page
    .locator(".head-actions")
    .getByRole("button", { name: "Re-summarize", exact: true });

  await expect(page.locator(bubble)).toHaveCount(0);
  await resummarize.hover();
  await expect(page.locator(bubble)).toHaveText("Re-summarize");

  // Somewhere harmless and far from the row.
  await page.mouse.move(5, 400);
  await expect(page.locator(bubble)).toHaveCount(0);
});

test("both switchers explain their glyphs, not just the loose buttons", async ({
  page,
}) => {
  await openMeeting(page);
  const row = page.locator(".head-actions");

  await row.getByRole("tab", { name: "Audio", exact: true }).hover();
  await expect(page.locator(bubble)).toHaveText("Audio");

  await row.getByRole("button", { name: "Live context", exact: true }).hover();
  await expect(page.locator(bubble)).toHaveText(
    "Live context from your connectors",
  );
});

test("no ENABLED control keeps a native title beside its tooltip", async ({
  page,
}) => {
  // Two tooltips for one control, ours immediately and the OS's a second
  // later, is the failure mode adopting the directive introduces if a call
  // site forgets to drop `title`. Nothing about the bubble's own behaviour
  // would reveal it.
  //
  // Scoped to enabled controls on purpose: a DISABLED button dispatches no
  // pointer or focus events, so `[appTooltip]` cannot fire on it and the
  // native `title` is deliberately kept as the only way it can still say why
  // it is dead (Edit, meeting-command-bar). The two are never live together.
  await openMeeting(page);
  const titled = await page
    .locator(".head-actions [title]:not(:disabled)")
    .evaluateAll((nodes) =>
      nodes.map((node) => node.getAttribute("title") ?? ""),
    );
  expect(titled).toEqual([]);
});

test("the bubble never leaves the viewport, even from the last icon in the row", async ({
  page,
}) => {
  await openMeeting(page);
  await page
    .locator(".head-actions")
    .getByRole("button", { name: "Ask", exact: true })
    .hover();
  await expect(page.locator(bubble)).toBeVisible();

  const fits = await page.evaluate(() => {
    const el = document.querySelector(".mur-tooltip");
    if (!el) return null;
    const box = el.getBoundingClientRect();
    return {
      insideX: box.left >= 0 && box.right <= window.innerWidth,
      insideY: box.top >= 0 && box.bottom <= window.innerHeight,
    };
  });
  expect(fits).toEqual({ insideX: true, insideY: true });
});

test("pressing a control dismisses its tooltip instead of leaving it over the result", async ({
  page,
}) => {
  await openMeeting(page);
  const ask = page
    .locator(".head-actions")
    .getByRole("button", { name: "Ask", exact: true });
  await ask.hover();
  await expect(page.locator(bubble)).toBeVisible();

  await ask.click();
  await expect(page.locator(".ask-drawer")).toBeVisible();
  // The pointer is still resting on the control, so only the explicit
  // click-dismiss can have removed this.
  await expect(page.locator(bubble)).toHaveCount(0);
});

test("a keyboard user gets the same explanation", async ({ page }) => {
  // NOT verifiable in the authoring environment: with the browser pane hidden,
  // `document.hasFocus()` is false and the page receives no focus events at
  // all — a listener attached directly to the button saw none either. This
  // test is the thing that actually exercises it, under a focused browser.
  await openMeeting(page);
  // The control is NAMED "More" (its `.sr-only` text) and EXPLAINED as "More
  // actions" (the tooltip). Conflating the two is what made the first version
  // of this test hunt for a button that does not exist.
  await page
    .locator(".head-actions")
    .getByRole("button", { name: "More", exact: true })
    .focus();
  await expect(page.locator(bubble)).toHaveText("More actions");

  await page.keyboard.press("Escape");
  await expect(page.locator(bubble)).toHaveCount(0);
});
