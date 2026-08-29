import { expect, test, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Settings must not assert that the local server for Claude is running when it is not.
 *
 * The screen said, in the present tense, "Murmur runs a small server on this Mac … at
 * 127.0.0.1:8765" and handed over a config to paste — unconditionally. A bind failure was a
 * `tracing::warn!` and a dead thread; nothing else in the app knew. Found 2026-08-28 on a real
 * machine where an unrelated `python -m http.server 8765` had held the port for two days: the
 * SHIPPED app's own log carried the same warning, and Settings still promised a running server.
 *
 * The config is the load-bearing half. A config that points at a dead port does not fail loudly —
 * it fails inside Claude, later, as "the tools just don't work", which is the hardest kind of
 * failure to trace back here.
 */
/** Navigate to the Privacy & Integrations card, the same way `privacy-honesty.spec.ts` does. */
async function openPrivacySettings(page: Page) {
  await page.goto("/settings");
  await page.getByText("Privacy & Integrations").first().click();
  const section = page.locator("app-settings-privacy-section");
  await expect(section.getByText("Local server for Claude")).toBeVisible({
    timeout: 10_000,
  });
  return section;
}

const CONFIG = JSON.stringify(
  { mcpServers: { murmur: { type: "http", url: "http://127.0.0.1:8765" } } },
  null,
  2,
);

test("a port held by another app is named, and the config is withheld", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      get_mcp_config: CONFIG,
      get_mcp_status: { state: "portInUse", port: 8765 },
    },
  );
  const section = await openPrivacySettings(page);

  await expect(section.getByRole("status")).toContainText(
    "another app on this Mac is using port 8765",
  );
  // Self-healing is promised because the listener really does retry — see mcp.rs's bind loop.
  await expect(section.getByRole("status")).toContainText("no restart needed");
  // Handing over a config that points at a dead port is worse than handing over none.
  await expect(
    section.getByRole("button", { name: /Copy config/ }),
  ).toBeDisabled();
});

test("a listening server keeps the config copyable", async ({ page }) => {
  await mockTauri(
    page,
    {},
    {
      get_mcp_config: CONFIG,
      get_mcp_status: { state: "listening", port: 8765 },
    },
  );
  const section = await openPrivacySettings(page);

  // The CONTROL arm. Without it the test above would pass even if the copy button were disabled
  // unconditionally, or the warning rendered always — proving nothing about the status binding.
  await expect(section.getByRole("status")).toHaveCount(0);
  await expect(section.getByRole("button", { name: /Copy config/ })).toBeEnabled();
});

test("an unavailable server says so rather than staying silent", async ({ page }) => {
  await mockTauri(
    page,
    {},
    {
      get_mcp_config: CONFIG,
      get_mcp_status: { state: "unavailable", port: 8765 },
    },
  );
  const section = await openPrivacySettings(page);

  await expect(section.getByRole("status")).toContainText("Not running yet");
  // A distinct instruction from the port case: this one does NOT recover on its own.
  await expect(section.getByRole("status")).toContainText("restart");
  await expect(
    section.getByRole("button", { name: /Copy config/ }),
  ).toBeDisabled();
});
