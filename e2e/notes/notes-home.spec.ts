import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Notes home — a normal in-flow route beside the ALWAYS-VISIBLE main sidebar
 * (2026-07-12; was a [note-folder rail | note list] drill-down that hid the
 * primary sidebar). The note-folder tree now lives IN the main sidebar
 * (`NotesSidebarTreeComponent`, nested under "Notes" — `AppShellComponent`);
 * this test confirms BOTH render: the sidebar tree (folders + "All notes")
 * AND the content pane's TABLE (2026-07-12, replaces the card grid — incl.
 * the masked locked row + a tag pill + the "New note" control), with NO
 * console/page errors — the runtime check that catches NG0600 / ɵcmp /
 * forwardRef regressions a green `ng build` misses.
 */
test("notes home renders the sidebar tree + the note table (incl. masked locked row) with no console errors", async ({
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

  // The ALWAYS-VISIBLE main sidebar renders too (Stage 1's whole point).
  await expect(page.locator("mur-sidebar.app-sidebar")).toBeVisible();

  // The prominent "New note" action.
  await expect(page.locator(".new-note-btn")).toBeVisible();

  // The sidebar's Notes section: the HEADER is the "all items" affordance
  // (the separate "All notes" root row was removed 2026-07-12 as a redundant
  // layer) + the tree lists both note folders directly.
  await expect(
    page.locator("mur-sidebar-section .nav-row-link", { hasText: "Notes" }),
  ).toBeVisible();
  const treeNames = page.locator("app-notes-sidebar-tree mur-tree-row .row-label");
  await expect(treeNames.filter({ hasText: "Work" })).toBeVisible();

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
