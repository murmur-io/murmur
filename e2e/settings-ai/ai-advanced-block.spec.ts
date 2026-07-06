import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Integration spec for AiAdvancedBlockComponent.
 *
 * Since the posture-first redesign the Default-engine select lives in the
 * ALWAYS-VISIBLE AiSetupBlockComponent — what the "⚙ Advanced" disclosure
 * hides is the Engines catalog (<app-ai-connection-cards />, heading
 * "Engines") and the per-feature overrides (<app-ai-role-rows />, the
 * "Customize per feature" button).
 */
test.describe("ai-advanced-block — cloud posture", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page, { brain_posture: () => "cloud" });
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
  });

  // ── (a) — collapsed by default ───────────────────────────────────────────
  test("(a) Advanced is collapsed by default — Engines catalog and per-feature overrides are hidden", async ({
    page,
  }) => {
    // The Engines heading (ai-connection-cards) and the per-feature button
    // (ai-role-rows) live inside the @if (expanded()) region — absent at load.
    await expect(
      page.getByRole("heading", { name: "Engines" }),
    ).not.toBeVisible();
    await expect(
      page.getByRole("button", { name: /Customize per feature/ }),
    ).not.toBeVisible();
    // The Default-engine select is NOT behind Advanced — it stays visible.
    await expect(
      page.locator('select[formcontrolname="providerId"]'),
    ).toBeVisible();
  });

  // ── (b) — toggle reveals everything ─────────────────────────────────────
  test("(b) clicking ⚙ Advanced reveals the Engines catalog and the Customize-per-feature button", async ({
    page,
  }) => {
    await page.getByRole("button", { name: /Advanced/ }).click();

    // The Engines heading comes from <app-ai-connection-cards /> rendered inside.
    await expect(page.getByRole("heading", { name: "Engines" })).toBeVisible();

    // The per-feature toggle button comes from <app-ai-role-rows />.
    await expect(
      page.getByRole("button", { name: /Customize per feature/ }),
    ).toBeVisible();
  });
});

// ── (c) — fully_local posture replaces the Default-engine card ────────────
test.describe("ai-advanced-block — fully_local posture", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page, { brain_posture: () => "fully_local" });
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
  });

  test("(c) Default-engine select is not rendered — the on-device setup card shows instead", async ({
    page,
  }) => {
    // setupCards() for "fully_local" is ["local"] — no "engine" card, so the
    // providerId select is gone and the on-device card copy renders.
    await expect(
      page.getByText("Everything runs on this Mac — nothing leaves."),
    ).toBeVisible();
    await expect(
      page.locator('select[formcontrolname="providerId"]'),
    ).toHaveCount(0);
  });
});
