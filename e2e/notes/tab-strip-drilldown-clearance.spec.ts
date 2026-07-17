import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Drill-down routes (/settings, /org-item) render NO sidebar/pill chrome, so
 * `mur-tab-strip` starts flush at the window's LEFT edge — exactly where the
 * overlay macOS traffic lights float (trafficLightPosition 32,30 → buttons
 * span ≈ x 32..84, vertically inside the 48px strip). The strip must reserve
 * left clearance there (`app-shell.drilldown` host class + the `:host-context`
 * rule in tab-strip.component.scss) while keeping its normal padding in
 * sidebar mode, where it sits right of the sidebar panel. Headless Playwright
 * has no real macOS window chrome, so this pins the CSS contract (the computed
 * clearance), not the OS pixels.
 */
test("the tab strip clears the traffic-light zone on drill-down routes", async ({
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

  // Sidebar mode: the strip sits right of the sidebar panel — normal padding
  // (--space-5 = 24px), no traffic lights anywhere near it.
  const strip = page.locator(".tab-strip");
  await expect(strip).toHaveCSS("padding-left", "24px");

  // Enter the Settings drill-down through the real sidebar affordance: the
  // chrome unmounts, the strip now starts at the window edge → clearance on.
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await expect(page).toHaveURL(/\/settings/);
  await expect(strip).toHaveCSS("padding-left", "96px");

  // Leaving the drill-down restores the normal padding.
  await page.getByRole("button", { name: "Back to Murmur" }).click();
  await expect(strip).toHaveCSS("padding-left", "24px");

  expect(consoleErrors).toEqual([]);
});
