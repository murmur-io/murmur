import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Notes home is an exact-list route: the persistent global rail chooses the
 * product surface while the adjacent Browse panel owns list navigation. The
 * content pane still renders the note table, including the sealed row, with
 * no console/page errors — the runtime check that catches NG0600 / ɵcmp /
 * forwardRef regressions a green `ng build` misses.
 */
test("notes home renders Browse navigation + the note table (incl. masked locked row) with no console errors", async ({
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
  await page.goto("/notes");

  // The content pane proves the route resolved.
  await expect(page.locator(".notes-content")).toBeVisible();

  const globalNavigation = page.getByRole("navigation", {
    name: "Global navigation",
  });
  await expect(globalNavigation).toBeVisible();

  const browseSidebar = page.getByRole("complementary", {
    name: "Browse sidebar",
  });
  await expect(browseSidebar).toBeVisible();
  await expect(
    browseSidebar.getByRole("link", { name: "Notes", exact: true }),
  ).toHaveClass(/active/);

  // The prominent "New note" action.
  await expect(page.locator(".new-note-btn")).toBeVisible();

  // Exact list routes do not mount the hierarchy panel or its former section
  // wrapper. The hierarchy is reserved for Space and leaf routes.
  await expect(page.locator("mur-sidebar.spaces-sidebar")).toHaveCount(0);
  await expect(page.locator("mur-sidebar-section")).toHaveCount(0);

  // The table renders (thead + the visible note row + its tag pill).
  await expect(page.locator(".mur-table thead th", { hasText: "Title" })).toBeVisible();
  await expect(page.getByText("My First Note")).toBeVisible();
  await expect(page.locator(".note-tag", { hasText: "idea" })).toBeVisible();

  // The masked (sealed-not-unlocked) note row shows the lock title, no snippet.
  await expect(page.getByText("🔒 Locked")).toBeVisible();

  // No NG0600 / ɵcmp / any other console error surfaced through the render.
  expect(consoleErrors).toEqual([]);
});

/**
 * Auto-organize drives end-to-end: the header button fetches a plan, the review
 * sheet opens (opaque T3), Apply calls `apply_organize_plan` and closes — all
 * with no console errors.
 */
test("auto-organize opens the review sheet and applies with no console errors", async ({
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
  await page.goto("/notes");
  await expect(page.locator(".notes-content")).toBeVisible();

  // Open the review sheet.
  await page.locator(".organize-btn").click();
  const sheet = page.locator("app-organize-sheet .sheet");
  await expect(sheet).toBeVisible();
  // The proposed move: "My First Note" → "Ideas" (a NEW folder).
  await expect(sheet.getByText("My First Note")).toBeVisible();
  await expect(sheet.locator(".move-new")).toHaveText("NEW");

  // Apply → the sheet closes (apply_organize_plan resolved).
  await sheet.locator(".sheet-actions .btn-primary").click();
  await expect(sheet).toBeHidden();

  expect(consoleErrors).toEqual([]);
});
