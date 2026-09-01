import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Settings opens as a MODAL over the whole window, and the shell underneath is
 * unchanged by it: the tab strip keeps its normal 24px inset rather than
 * switching to a flush-left drill-down clearance, and it still has that inset
 * after the dialog closes.
 *
 * The geometry assertion used to be the opposite one — the settings surface had
 * to BEGIN after the global rail, because it was a pane beside it. It now
 * covers the rail deliberately, so what is checked is that the dialog spans the
 * window while the strip's own padding is left alone.
 */
test("settings opens as a modal over the shell and leaves the tab strip alone", async ({
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

  const [railBox, scrimBox] = await Promise.all([
    globalNavigation.boundingBox(),
    page.locator("app-settings .settings-scrim").boundingBox(),
  ]);
  expect(railBox).not.toBeNull();
  expect(scrimBox).not.toBeNull();
  // The dialog spans the window — it starts at or before the rail and ends
  // after it, rather than beginning where the rail stops.
  expect(scrimBox!.x).toBeLessThanOrEqual(railBox!.x);
  expect(scrimBox!.x + scrimBox!.width).toBeGreaterThanOrEqual(
    railBox!.x + railBox!.width,
  );

  // Leaving Settings preserves the same shell-level inset.
  await page.getByRole("button", { name: "Close settings" }).click();
  await expect(strip).toHaveCSS("padding-left", "24px");

  expect(consoleErrors).toEqual([]);
});
