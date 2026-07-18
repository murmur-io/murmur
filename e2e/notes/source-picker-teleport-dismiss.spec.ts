import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Regression (2026-07-18, note-surface redesign Phase 1): the `<mur-source-picker>`
 * popover + scrim are TELEPORTED to `<body>` (`appTeleportToBody`) so their
 * `position: fixed` anchors to the VIEWPORT instead of the `.chat.card`
 * backdrop-filter containing block (which offset it off-anchor — "looked dead").
 *
 * The teleport must ALSO stay dismissable: a prior revision left the popover
 * UNDISMISSABLE because the directive moved the node back to its in-tree slot on
 * destroy — but Angular's `detachView` removes DOM nodes (via their CURRENT parent,
 * `body`) BEFORE destroy hooks run, so the move-back RESURRECTED the just-removed
 * popover. This asserts: open → teleported to <body> + on-anchor → dismiss via
 * outside-click (scrim) AND via Escape → the popover is gone with ZERO orphan
 * overlay nodes left in the document. RED on the move-back revision, GREEN now.
 */
test("source-picker teleports on-anchor and stays dismissable (scrim + Escape), no orphan", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") consoleErrors.push(m.text());
  });
  page.on("pageerror", (e) => consoleErrors.push(String(e)));

  await mockNotes(page, {
    list_links: () => [],
  });
  await page.goto("/notes/n1");

  const picker = page.locator("app-note-chat mur-source-picker");
  await expect(picker).toBeVisible();
  const trigger = picker.locator(".sp-trigger");

  // --- Open: the popover mounts, TELEPORTED to <body>, ON-ANCHOR under the trigger.
  await trigger.click();
  const pop = page.locator(".sp-pop");
  await expect(pop).toBeVisible();

  const teleportParent = await page.evaluate(
    () => document.querySelector(".sp-overlay")?.parentElement?.tagName ?? null,
  );
  expect(teleportParent).toBe("BODY");

  const geo = await page.evaluate(() => {
    const t = document
      .querySelector("app-note-chat .sp-trigger")!
      .getBoundingClientRect();
    const p = document.querySelector(".sp-pop")!.getBoundingClientRect();
    return { dLeft: Math.abs(p.left - t.left), below: p.top >= t.top };
  });
  // On-anchor: aligned to the trigger's left and below it — NOT shoved hundreds of
  // px away by the card's box (the containing-block bug the teleport fixes).
  expect(geo.dLeft).toBeLessThan(60);
  expect(geo.below).toBe(true);

  // --- Dismiss #1: outside click (the full-viewport scrim) → gone, no orphan.
  await page.mouse.click(4, 4);
  await expect(pop).toHaveCount(0);
  expect(
    await page.evaluate(
      () => document.querySelectorAll(".sp-overlay, .sp-pop, .sp-scrim").length,
    ),
  ).toBe(0);

  // --- Dismiss #2: Escape from the search field → gone, no orphan.
  await trigger.click();
  await expect(pop).toBeVisible();
  await page.locator(".sp-search").focus();
  await page.keyboard.press("Escape");
  await expect(pop).toHaveCount(0);
  expect(
    await page.evaluate(
      () => document.querySelectorAll(".sp-overlay, .sp-pop, .sp-scrim").length,
    ),
  ).toBe(0);

  expect(consoleErrors).toEqual([]);
});
