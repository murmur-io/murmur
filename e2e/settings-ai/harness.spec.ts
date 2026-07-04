import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

test("harness boots the settings page under the mocked Tauri invoke", async ({
  page,
}) => {
  await mockTauri(page);
  await page.goto("/settings");
  // The settings nav rendering proves the app booted under the mock + the route resolved.
  await expect(page.getByText("AI & Models").first()).toBeVisible();
  // Open the AI & Models section; its block header proves the section rendered.
  await page.getByText("AI & Models").first().click();
  await expect(page.getByText("What Murmur uses")).toBeVisible();
});
