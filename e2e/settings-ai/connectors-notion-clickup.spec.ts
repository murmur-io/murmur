import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Settings ▸ Connectors — the two BYO-token READ connectors (Notion, ClickUp).
 *
 * RED contract (fails before the connectors ship):
 *   (a) the two master toggles don't exist in the Connectors section at all;
 *   (b) with the toggle ON, the token fieldset + one-time consent row don't render;
 *   (c) `set_notion_token` / `consent_to_notion` (and the ClickUp pair) are never invoked, so the
 *       "Token set ✓" / "access allowed" pills never appear.
 *
 * SECURITY assertion carried by (c): the token input is a password field, the value is CLEARED
 * after saving, and the token string is NEVER rendered back anywhere in the DOM — the UI only ever
 * reports set / not-set, exactly like the Jira and Slack token UI.
 */

const NOTION_TOKEN = "ntn_e2e_SECRET_NOTION_TOKEN";
const CLICKUP_TOKEN = "pk_e2e_SECRET_CLICKUP_TOKEN";

/**
 * Page-side command overrides. Self-contained by contract (they are serialized to strings), so all
 * state lives on `window`. `has_*` reads the same slot `set_*` writes, which is what makes the
 * "Token set ✓" flip a real round-trip through IPC rather than a local UI guess.
 */
const OVERRIDES = {
  set_notion_token: (args: any) => {
    (window as any).__notionToken = args.key;
    return null;
  },
  has_notion_token: () => Boolean((window as any).__notionToken),
  consent_to_notion: () => {
    (window as any).__notionConsent = true;
    return null;
  },
  set_clickup_token: (args: any) => {
    (window as any).__clickupToken = args.key;
    return null;
  },
  has_clickup_token: () => Boolean((window as any).__clickupToken),
  consent_to_clickup: () => {
    (window as any).__clickupConsent = true;
    return null;
  },
  save_config: (args: any) => {
    (window as any).__savedConfig = args.config ?? args;
    return null;
  },
};

/** Seed the demo mock's `get_config` with the connector flags this spec needs. */
async function seedConfig(
  page: import("@playwright/test").Page,
  cfg: Record<string, unknown>,
): Promise<void> {
  await page.addInitScript((c: Record<string, unknown>) => {
    (window as any).__demoConfig = Object.assign(
      {},
      (window as any).__demoConfig ?? {},
      c,
    );
  }, cfg);
}

async function openConnectors(
  page: import("@playwright/test").Page,
): Promise<void> {
  await page.goto("/settings");
  await page.getByText("Connectors").first().click();
  await expect(page.locator("app-settings-connectors-section")).toBeVisible({
    timeout: 10_000,
  });
}

test.describe("Settings ▸ Connectors — Notion + ClickUp", () => {
  // ── (a) both master toggles render, OFF by default, with their detail collapsed ──
  test("(a) the Notion and ClickUp toggles render and default to collapsed/off", async ({
    page,
  }) => {
    await mockTauri(page, OVERRIDES);
    await openConnectors(page);

    await expect(page.getByText("Notion", { exact: true })).toBeVisible();
    await expect(page.getByText("ClickUp", { exact: true })).toBeVisible();
    // Off ⇒ no token fieldset, no consent affordance, no egress banner for them.
    await expect(page.getByText("Notion integration token")).toHaveCount(0);
    await expect(page.getByText("ClickUp API token")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Allow Notion access (one-time consent)" }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", {
        name: "Allow ClickUp access (one-time consent)",
      }),
    ).toHaveCount(0);
  });

  // ── (b)+(c) Notion: token set → pill flips; consent → pill flips; token never shown ──
  test("(b) Notion token save + one-time consent persist through IPC without ever showing the token", async ({
    page,
  }) => {
    await mockTauri(page, OVERRIDES);
    await seedConfig(page, { notionEnabled: true });
    await openConnectors(page);

    // The enabled connector reveals its BYO-token fieldset, starting "Not set".
    const tokenInput = page.getByPlaceholder(
      "Notion integration token (ntn_… / secret_…)",
    );
    await expect(tokenInput).toBeVisible({ timeout: 10_000 });
    await expect(tokenInput).toHaveAttribute("type", "password");
    const fieldset = page.locator("fieldset", { has: tokenInput });
    await expect(fieldset.getByText("Not set")).toBeVisible();

    await tokenInput.fill(NOTION_TOKEN);
    await fieldset.getByRole("button", { name: "Save token" }).click();

    // The pill flips only because `has_notion_token` now sees what `set_notion_token` stored.
    await expect(fieldset.getByText("Token set ✓")).toBeVisible();
    expect(await page.evaluate(() => (window as any).__notionToken)).toBe(
      NOTION_TOKEN,
    );
    // The input is cleared and the token appears NOWHERE in the rendered DOM.
    await expect(tokenInput).toHaveValue("");
    expect(await page.content()).not.toContain(NOTION_TOKEN);

    // One-time consent goes through the dedicated command (never a settings save).
    await page
      .getByRole("button", { name: "Allow Notion access (one-time consent)" })
      .click();
    await expect(page.getByText("Notion access allowed")).toBeVisible();
    expect(await page.evaluate(() => (window as any).__notionConsent)).toBe(
      true,
    );
  });

  // ── (b)+(c) ClickUp: workspace id + token + consent ──
  test("(c) ClickUp workspace id, token save and one-time consent persist through IPC", async ({
    page,
  }) => {
    await mockTauri(page, OVERRIDES);
    await seedConfig(page, { clickupEnabled: true, clickupTeamId: "9001" });
    await openConnectors(page);

    // The non-secret workspace id round-trips from the config into the form.
    const teamId = page.getByPlaceholder("Workspace (team) id, e.g. 9001");
    await expect(teamId).toBeVisible({ timeout: 10_000 });
    await expect(teamId).toHaveValue("9001");

    const tokenInput = page.getByPlaceholder(
      "ClickUp personal API token (pk_…)",
    );
    await expect(tokenInput).toHaveAttribute("type", "password");
    const fieldset = page.locator("fieldset", { has: tokenInput });
    await expect(fieldset.getByText("Not set")).toBeVisible();

    await tokenInput.fill(CLICKUP_TOKEN);
    await fieldset.getByRole("button", { name: "Save token" }).click();

    await expect(fieldset.getByText("Token set ✓")).toBeVisible();
    expect(await page.evaluate(() => (window as any).__clickupToken)).toBe(
      CLICKUP_TOKEN,
    );
    await expect(tokenInput).toHaveValue("");
    expect(await page.content()).not.toContain(CLICKUP_TOKEN);

    await page
      .getByRole("button", { name: "Allow ClickUp access (one-time consent)" })
      .click();
    await expect(page.getByText("ClickUp access allowed")).toBeVisible();
    expect(await page.evaluate(() => (window as any).__clickupConsent)).toBe(
      true,
    );
  });

  // ── The loud egress warning is impossible to miss on both new connectors ──
  test("(d) enabling either connector surfaces the off-device egress warning", async ({
    page,
  }) => {
    await mockTauri(page, OVERRIDES);
    await seedConfig(page, { notionEnabled: true, clickupEnabled: true });
    await openConnectors(page);

    const banners = page.locator("p.connector-egress");
    await expect(banners.filter({ hasText: "Notion search" })).toBeVisible({
      timeout: 10_000,
    });
    await expect(banners.filter({ hasText: "ClickUp search" })).toBeVisible();
  });
});
