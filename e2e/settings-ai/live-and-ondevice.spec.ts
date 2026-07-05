import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Integration spec for DuringMeetingsBlockComponent +
 * OnDeviceIntelligenceBlockComponent (Task 5).
 *
 * RED contract:
 *   (a)–(b) The custom elements don't exist in the DOM before the components
 *       are created → `toBeAttached()` fails = RED.
 *   (c) The assistant + proactive-hints checkboxes are not present before
 *       Task 5 → `toBeAttached()` fails = RED.
 *   (d) The semantic-search toggle + Re-index button are not present before
 *       Task 5 → fails = RED.
 *   (e) The cloud-consent warning doesn't render before the component →
 *       `toBeVisible()` fails = RED.
 */
test.describe("during-meetings-block + on-device-intelligence-block", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page);
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
  });

  // ── (a) — app-during-meetings-block custom element is mounted ────────────
  test("(a) app-during-meetings-block custom element is in the DOM", async ({
    page,
  }) => {
    await expect(page.locator("app-during-meetings-block")).toBeAttached();
  });

  // ── (b) — app-on-device-intelligence-block custom element is mounted ─────
  test("(b) app-on-device-intelligence-block custom element is in the DOM", async ({
    page,
  }) => {
    await expect(page.locator("app-on-device-intelligence-block")).toBeAttached();
  });

  // ── (c) — assistant + proactive-hints toggles render ────────────────────
  // The checkboxes became design-system <mur-toggle> controls (CVA), so the
  // formControlName sits on the mur-toggle element, not a native input.
  test("(c) in-meeting assistant and proactive-hints toggles render", async ({
    page,
  }) => {
    await expect(
      page.locator('mur-toggle[formcontrolname="realtimeReactions"]'),
    ).toBeAttached();
    await expect(
      page.locator('mur-toggle[formcontrolname="proactiveHintsEnabled"]'),
    ).toBeAttached();
  });

  // ── (d) — semantic-search toggle + Re-index notes button render ──────────
  test("(d) semantic-search toggle and Re-index notes button render", async ({
    page,
  }) => {
    await expect(
      page.locator('mur-toggle[formcontrolname="semanticSearchEnabled"]'),
    ).toBeAttached();
    await expect(
      page.getByRole("button", { name: "Re-index notes" }),
    ).toBeVisible();
  });
});

// ── (e) — cloud-consent warning appears when conditions are met ───────────
test("(e) cloud-consent warning appears when realtime on + live target is cloud + not consented", async ({
  page,
}) => {
  // The banner requires: posture ≠ cloud (mock default is "hybrid"), realtime
  // reactions ON (mock default), an explicitly cloud-routed live target
  // (roleLiveConnection: claude_code — explicit override wins the resolver),
  // and cloud egress NOT yet consented. `__demoConfig` merges over the base
  // mock's DEFAULT_CONFIG, so only the deltas are pinned here.
  await mockTauri(page);
  await page.addInitScript(() => {
    (window as unknown as { __demoConfig: Record<string, unknown> }).__demoConfig =
      {
        realtimeReactions: true,
        roleLiveConnection: "claude_code",
        cloudEgressConsented: false,
      };
  });
  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();
  // The banner text that appears when consent is needed.
  await expect(
    page.getByText(/sends live meeting context to your provider/),
  ).toBeVisible({ timeout: 10_000 });
});
