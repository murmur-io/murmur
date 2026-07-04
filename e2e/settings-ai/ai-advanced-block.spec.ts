import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Integration spec for AiAdvancedBlockComponent (Task 4).
 *
 * RED contract:
 *   (a) Before the component exists the Default-AI select is always visible
 *       (inside the old ai-defaults-block). After, it lives behind the toggle
 *       → `not.toBeVisible()` FAILS before Task 4 and PASSES after.
 *   (b) The "⚙ Advanced" toggle button does not exist before Task 4 — the
 *       `.click()` times out / throws before implementation.
 *   (c) Same — the Advanced button does not exist before Task 4.
 */
test.describe("ai-advanced-block — cloud posture (default)", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page);
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
  });

  // ── (a) — collapsed by default ───────────────────────────────────────────
  test("(a) Advanced is collapsed by default — Default-AI select is not visible", async ({
    page,
  }) => {
    // The Default-AI select lives inside the @if (expanded()) region.
    // Before Task 4 the select was always visible → this assertion FAILS = RED.
    await expect(
      page.locator('select[formcontrolname="providerId"]'),
    ).not.toBeVisible();
  });

  // ── (b) — toggle reveals everything ─────────────────────────────────────
  test("(b) clicking ⚙ Advanced reveals the Providers section, Default-AI select, and Customize-per-feature button", async ({
    page,
  }) => {
    // Before Task 4 this button does not exist → click times out = RED.
    await page.getByRole("button", { name: /Advanced/ }).click();

    // Default-AI select is now in the DOM and visible.
    await expect(
      page.locator('select[formcontrolname="providerId"]'),
    ).toBeVisible();

    // The Providers heading comes from <app-ai-connection-cards /> rendered inside.
    await expect(page.getByRole("heading", { name: "Providers" })).toBeVisible();

    // The per-feature toggle button comes from <app-ai-role-rows />.
    await expect(
      page.getByRole("button", { name: /Customize per feature/ }),
    ).toBeVisible();
  });
});

// ── (c) — fully_local posture disables the Default-AI select ────────────
test.describe("ai-advanced-block — fully_local posture", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page, { brain_posture: () => "fully_local" });
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
    // Before Task 4 this button does not exist → click times out = RED.
    await page.getByRole("button", { name: /Advanced/ }).click();
  });

  test("(c) Default-AI select is disabled and the fully-local note is visible", async ({
    page,
  }) => {
    await expect(
      page.locator('select[formcontrolname="providerId"]'),
    ).toBeDisabled();
    await expect(
      page.getByText(/Not used.*Fully local/),
    ).toBeVisible();
  });
});
