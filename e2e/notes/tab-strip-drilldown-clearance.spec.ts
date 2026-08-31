import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Settings keeps the persistent global rail. The tab strip therefore retains
 * its normal 24px inset instead of switching to a flush-left drill-down
 * clearance, and the fixed settings surface must begin after the rail.
 */
test("settings keeps normal tab-strip padding and clears the global rail", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page);

  // Put a tab on the strip through the real in-app open action (a goto
  // deep-link deliberately opens no tab — see link-picker.spec.ts).
  await page.goto("/notes");
  await page.getByRole("button", { name: "My First Note" }).click();
  await expect(page.locator(".tab-strip .tab-item")).toHaveCount(1);

  // Browse mode uses the normal inset (--space-5 = 24px).
  const strip = page.locator(".tab-strip");
  await expect(strip).toHaveCSS("padding-left", "24px");

  const globalNavigation = page.getByRole("navigation", {
    name: "Primary navigation",
  });
  await globalNavigation
    .getByRole("link", { name: "Settings", exact: true })
    .click();
  await expect(page).toHaveURL(/\/settings/);
  await expect(globalNavigation).toBeVisible();
  await expect(strip).toHaveCSS("padding-left", "24px");

  const [railBox, settingsBox] = await Promise.all([
    globalNavigation.boundingBox(),
    page.locator("app-settings .settings-shell").boundingBox(),
  ]);
  expect(railBox).not.toBeNull();
  expect(settingsBox).not.toBeNull();
  expect(settingsBox!.x).toBeGreaterThanOrEqual(railBox!.x + railBox!.width);

  // Leaving Settings preserves the same shell-level inset.
  await page.getByRole("button", { name: "Back to Murmur" }).click();
  await expect(strip).toHaveCSS("padding-left", "24px");

  expect(consoleErrors).toEqual([]);
});
