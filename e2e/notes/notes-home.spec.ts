import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Notes home — the [note-folder rail | note list] drill view. Confirms the page
 * boots under the mocked IPC and renders the note cards (incl. the masked locked
 * row + a tag pill + the "New note" control) with NO console/page errors — the
 * runtime check that catches NG0600 / ɵcmp / forwardRef regressions a green
 * `ng build` misses.
 */
test("notes home renders folders + note cards (incl. masked locked row) with no console errors", async ({
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

  // The content pane proves the drill view booted + the route resolved.
  await expect(page.locator(".notes-content")).toBeVisible();

  // The prominent "New note" action.
  await expect(page.locator(".new-note-btn")).toBeVisible();

  // The folder rail lists both note folders (+ the "All notes" root).
  const folderNames = page.locator(".folder-name");
  await expect(folderNames.filter({ hasText: "All notes" })).toBeVisible();
  await expect(folderNames.filter({ hasText: "Work" })).toBeVisible();

  // The visible note card + its tag pill.
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
