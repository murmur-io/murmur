import { test, expect } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * `/library` renders a WINDOW of its rows, with a way out.
 *
 * `list_meetings` already caps its read at 200, so this is not about the fetch — it is about the
 * DOM. Each row carries links, badges and a date, so a few hundred is thousands of nodes that
 * zoneless change detection walks on every pass, and the cost lands on scrolling and typing rather
 * than on load. Same shape and the same number as `audio-panel`'s transcript window.
 *
 * The second assertion is the one that makes this a fix rather than a regression: the window MUST
 * be escapable. Hiding rows with no way to reach them is a loss of function dressed up as a
 * performance win, and a test that only checked the cap would happily accept it.
 */
const ROW_COUNT = 150;

test("the meetings list windows its rows and can be expanded to all of them", async ({
  page,
}) => {
  // The handler is SERIALIZED into the page, so it cannot close over anything from this file —
  // a `() => ROWS` referencing a module const here evaluates to `undefined` in the browser, the
  // call rejects, `Promise.allSettled` swallows it, and the list renders empty while the test
  // reads that as "the window worked". The data is built inside the handler for that reason.
  await mockTauri(page, {
    list_meetings: () =>
      Array.from({ length: 150 }, (_, i) => ({
        id: `m${i}`,
        title: `Meeting ${i}`,
        startedAt: "2026-09-01T10:00:00Z",
        endedAt: "2026-09-01T10:10:00Z",
        durationS: 600,
        audioPath: null,
        status: "done",
        folderId: null,
      })),
    search_meetings: () => [],
  });
  await page.goto("/library");

  // The row is an `<a class="row">` driven by (click), not an href — a link-shaped locator
  // silently matches nothing here and the test would pass by finding zero of everything.
  const rows = page.locator("ul.list li a.row");
  // Wait for the first row rather than counting straight after `goto`: an immediate count reads 0
  // while the list is still loading, and `toBeLessThanOrEqual(80)` is perfectly happy with 0 —
  // which is why the lower bound is asserted too.
  await expect(rows.first()).toBeVisible({ timeout: 10_000 });
  const windowed = await rows.count();
  expect(windowed).toBeLessThanOrEqual(80);
  expect(windowed).toBeGreaterThan(0);

  const showAll = page.getByRole("button", { name: /Show all/ });
  await expect(showAll).toBeVisible();
  await showAll.click();

  // Every row is reachable once asked for — the window is a render budget, never a data ceiling.
  await expect(rows).toHaveCount(ROW_COUNT);
  await expect(showAll).toHaveCount(0);
});
