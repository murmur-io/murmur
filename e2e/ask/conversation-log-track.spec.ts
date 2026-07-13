import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Regression for the Ask conversation log's `@for` tracking. The turns array is
 * NOT append-only: `retry()` (ask.component.ts) pops the dangling user turn and
 * `send()` re-appends it, landing back at the same array INDEX it previously
 * occupied. `AskTurn` carries a stable `id` (minted by a monotonic counter) and
 * the template tracks `turn.id` — never `$index` — precisely so Angular mints a
 * FRESH DOM node for the retried bubble instead of reusing the old one (which
 * would silently skip its one-shot `bubble-in` entrance animation, the
 * `.ask-row` CSS rule in ask.component.scss).
 *
 * The FIRST question succeeds so the log already has a settled turn at index 0
 * BEFORE the one under test — this way `retry()`'s pop-then-reappend of the
 * SECOND (failing) turn never drives the whole `@for` through an empty array
 * (which would force a destroy/recreate regardless of tracking key and make
 * the test tracking-key-insensitive). `ask_vault` is mocked to succeed on the
 * first call, fail on the second, then succeed again on the retry.
 */
test("Ask conversation log mints a fresh DOM node for a retried turn (not an index-reused one)", async ({
  page,
}) => {
  await mockTauri(page, {
    list_meetings: () => [],
    list_notes: () => [
      {
        id: "n1",
        title: "A note",
        folderId: null,
        snippet: "",
        tags: [],
        updatedAt: 1_720_000_000_000,
        createdAt: 1_719_000_000_000,
        locked: false,
        shared: false,
      },
    ],
    // Overrides run PAGE-SIDE, re-parsed from a serialized string (see
    // mockTauri) — no closures over outer scope, so the call counter is
    // stashed on `window` instead.
    ask_vault: () => {
      const w = window as unknown as { __askVaultCalls?: number };
      w.__askVaultCalls = (w.__askVaultCalls ?? 0) + 1;
      if (w.__askVaultCalls === 2) {
        throw new Error("simulated ask_vault failure");
      }
      return {
        answer: "Answer #" + w.__askVaultCalls,
        sources: [],
        citations: [],
      };
    },
  });

  await page.goto("/ask");

  const input = page.locator(".ask-input");
  await expect(input).toBeVisible();

  // Turn 1: succeeds — settles a turn at index 0/1 before the one under test.
  await input.fill("First question");
  await input.press("Enter");
  await expect(
    page.locator(".ask-row.is-assistant .ask-bubble").last(),
  ).toContainText("Answer #1");

  // Turn 2: fails — the user bubble for it lands at index 2 and stays put.
  await input.fill("Second question that will fail");
  await input.press("Enter");
  const retryBtn = page.getByRole("button", { name: "Retry" });
  await expect(retryBtn).toBeVisible();

  const userRows = page.locator(".ask-row.is-user");
  await expect(userRows).toHaveCount(2);
  const failedUserRow = userRows.nth(1);
  await expect(failedUserRow).toContainText("Second question that will fail");

  // Stamp the failed turn's DOM node so we can tell whether Retry's
  // pop-then-reappend reuses it (index-tracked bug) or replaces it
  // (id-tracked fix). The array never goes empty across this retry (turn 0
  // stays put), so `@for` only has the tracking key to decide reuse.
  await failedUserRow.evaluate((el) => el.setAttribute("data-orig-node", "1"));

  await retryBtn.click();

  // Wait for the retry's async round-trip to fully land (the third
  // ask_vault call succeeds) before inspecting the DOM — `retry()` pops the
  // turn synchronously but `send()` re-appends + resolves asynchronously.
  await expect(page.locator(".ask-row.is-assistant .ask-bubble").last()).toContainText(
    "Answer #3",
  );

  // The retried turn lands back at the SAME array index (1) as the failed one.
  await expect(page.locator(".ask-row.is-user").nth(1)).toContainText(
    "Second question that will fail",
  );

  // Is the ORIGINAL stamped node (element identity, not just the attribute)
  // still connected to the document? A correctly `turn.id`-tracked `@for`
  // destroys the old node and creates a NEW one at the same slot — the
  // original element must be DETACHED. Under the banned `track $index`, the
  // very same element would be reused/kept connected.
  const stillConnected = await page.evaluate(() => {
    const el = document.querySelector('[data-orig-node="1"]');
    return el !== null && el.isConnected;
  });
  expect(stillConnected).toBe(false);
});
