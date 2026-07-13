import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Analytics audit fix — a real `get_analytics` failure must NOT render as the
 * "Nothing to measure yet" empty state.
 *
 * Pre-fix, `AnalyticsComponent.ngOnInit()` wrapped the `getAnalytics()` await in a
 * bare `try { ... } finally { ... }` with no `catch`: a rejected promise left
 * `data` at its initial `null`, `loading` still flipped to `false` in `finally`,
 * and `isEmpty()` (`!a || a.totalMeetings === 0`) evaluated `true` — so a genuine
 * backend error rendered byte-identical to a fresh, never-recorded vault, with no
 * way to tell the two apart and no retry affordance.
 *
 * RED contract: on the pre-fix code, forcing `get_analytics` to throw still shows
 * "Nothing to measure yet" (the `getByText(/Nothing to measure yet/)` expectation
 * would pass and the "Couldn't load your analytics" / Retry assertions below would
 * fail — there is no error branch at all).
 */
test.describe("Analytics — a real getAnalytics() failure surfaces as an error state (not empty)", () => {
  test("shows an error card with Retry, not the empty-vault state", async ({
    page,
  }) => {
    await mockTauri(page, {
      // Count invocations PAGE-SIDE (the override is serialized — no test-scope closures).
      get_analytics: () => {
        const w = window as unknown as { __analyticsCalls?: number };
        w.__analyticsCalls = (w.__analyticsCalls ?? 0) + 1;
        throw new Error("db locked");
      },
    });

    await page.goto("/analytics");

    // The failed load must NOT render as the empty-vault state.
    await expect(
      page.getByText("Nothing to measure yet"),
    ).not.toBeVisible();

    // It must instead surface a distinct, honest error state with a retry affordance.
    await expect(
      page.getByText("Couldn’t load your analytics"),
    ).toBeVisible({ timeout: 10_000 });
    const retry = page.getByRole("button", { name: "Retry" });
    await expect(retry).toBeVisible();

    // Retry re-invokes the same IPC command.
    await retry.click();
    await expect
      .poll(
        () =>
          page.evaluate(
            () =>
              (window as unknown as { __analyticsCalls?: number })
                .__analyticsCalls ?? 0,
          ),
        { timeout: 5_000 },
      )
      .toBeGreaterThanOrEqual(2);
  });
});
