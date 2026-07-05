import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Asserts the final AI & Models section order (Task 5 / Task 6 rewire):
 *
 *   1. Brain posture block       (app-brain-posture-block)
 *   2. Advanced disclosure block (app-ai-advanced-block, COLLAPSED by default)
 *   3. During-meetings block     (app-during-meetings-block)
 *   4. On-device intelligence    (app-on-device-intelligence-block)
 *   5. Privacy strip             (app-ai-privacy-strip)
 *
 * RED contract:
 *   (a) Before Task 5 the two new custom elements don't exist → the ordering
 *       assertions fail = RED.
 *   (b) "Enable Murmur Brain Live" is absent even before Task 5 (was already
 *       removed in Task 2); this assertion is a regression guard.
 *   (c) The Advanced disclosure is collapsed at load (its Engines catalog is
 *       hidden) while the Default-engine select stays visible in the
 *       always-shown ai-setup-block.
 */
test.describe("AI & Models section — final render order", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page);
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
  });

  // ── (a) — DOM order: posture → advanced → during → ondevice → privacy ────
  test("(a) five blocks render in correct top-to-bottom DOM order", async ({
    page,
  }) => {
    // Wait for the section to stabilise (async config load).
    await expect(page.locator("app-brain-posture-block")).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator("app-ai-advanced-block")).toBeVisible();
    await expect(page.locator("app-during-meetings-block")).toBeVisible();
    await expect(page.locator("app-on-device-intelligence-block")).toBeVisible();
    await expect(page.locator("app-ai-privacy-strip")).toBeVisible();

    // Verify DOM order using compareDocumentPosition (host elements have
    // `display: contents` so boundingBox() returns null — DOM position is
    // the authoritative check here).
    const inOrder = await page.evaluate(() => {
      const selectors = [
        "app-brain-posture-block",
        "app-ai-advanced-block",
        "app-during-meetings-block",
        "app-on-device-intelligence-block",
        "app-ai-privacy-strip",
      ];
      const els = selectors.map((s) => document.querySelector(s));
      if (els.some((el) => !el)) return false;
      for (let i = 0; i < els.length - 1; i++) {
        const pos = els[i]!.compareDocumentPosition(els[i + 1]!);
        // DOCUMENT_POSITION_FOLLOWING = 4 — el[i+1] comes AFTER el[i].
        if (!(pos & Node.DOCUMENT_POSITION_FOLLOWING)) return false;
      }
      return true;
    });
    expect(inOrder).toBe(true);
  });

  // ── (b) — "Enable Murmur Brain Live" is absent ───────────────────────────
  test('(b) "Enable Murmur Brain Live" text is absent', async ({ page }) => {
    await expect(page.getByText("Enable Murmur Brain Live")).toHaveCount(0);
  });

  // ── (c) — Advanced is collapsed at load; the Default-engine select is not ─
  // behind it (it lives in the always-visible ai-setup-block since the
  // posture-first redesign).
  test("(c) Advanced is collapsed at load while the Default-engine select stays visible", async ({
    page,
  }) => {
    // Wait for the async config load to settle first, so the negative
    // assertion below can't pass vacuously against a half-rendered section.
    await expect(
      page.locator('select[formcontrolname="providerId"]'),
    ).toBeVisible({ timeout: 10_000 });
    // The Engines catalog (inside the Advanced disclosure) stays hidden.
    await expect(
      page.getByRole("heading", { name: "Engines" }),
    ).not.toBeVisible();
  });
});
