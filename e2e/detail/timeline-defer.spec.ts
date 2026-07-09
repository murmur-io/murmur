import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * P0.1 (OOM fix) — the on-open timeline trigger is GONE.
 *
 * The 1h-meeting Detail-open OOM was rooted in `detail.component.ts loadMeeting()`
 * calling `void this.loadTimeline()` UNCONDITIONALLY on every open — firing
 * `get_timeline` (→ on a local Notes role, a multi-GB model load + a whole-transcript
 * prompt) even though the timeline only renders on the Audio tab (default tab = Note).
 *
 * The fix defers `loadTimeline()` to an effect keyed on `activeTab() === 'audio'`.
 * This spec pins that behavior against the REAL FE bundle (only the Tauri IPC boundary
 * is mocked): `get_timeline` must NOT be invoked while the Note tab is showing, and must
 * fire exactly ONCE when the user opens the Audio tab.
 *
 * RED contract: on the pre-fix code `loadMeeting()` calls `loadTimeline()` immediately,
 * so `__tlCalls` would already be 1 right after open → the `toBe(0)` assertion fails.
 */
test.describe("Detail — timeline generation deferred to the Audio tab (P0.1)", () => {
  test("get_timeline is NOT called on Note-tab open, fires once on Audio", async ({
    page,
  }) => {
    await mockTauri(page, {
      // Count invocations PAGE-SIDE (the override is serialized — no test-scope closures)
      // and return a minimal-but-valid MeetingTimeline so the Audio tab renders cleanly.
      get_timeline: () => {
        const w = window as unknown as { __tlCalls?: number };
        w.__tlCalls = (w.__tlCalls ?? 0) + 1;
        return { speakers: [], topics: [] };
      },
    });

    await page.goto("/meeting/m-atlas-roadmap");

    // The detail landed (its tab bar rendered) — default tab is Note.
    await expect(page.getByRole("tab", { name: "Audio" })).toBeVisible({
      timeout: 10_000,
    });

    // P0.1: opening the meeting (Note tab) must NOT trigger timeline generation.
    // Give any stray on-open effect a beat to (fail to) fire.
    await page.waitForTimeout(600);
    expect(
      await page.evaluate(
        () => (window as unknown as { __tlCalls?: number }).__tlCalls ?? 0,
      ),
    ).toBe(0);

    // Opening the Audio tab defer-loads the timeline exactly once.
    await page.getByRole("tab", { name: "Audio" }).click();
    await expect
      .poll(
        () =>
          page.evaluate(
            () => (window as unknown as { __tlCalls?: number }).__tlCalls ?? 0,
          ),
        { timeout: 5_000 },
      )
      .toBe(1);
  });
});
