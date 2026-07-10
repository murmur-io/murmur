import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * The note editor — loads a note via `get_note` and renders the title, body,
 * formatting toolbar and Edit/Preview toggle. Toggling to Preview renders the
 * markdown (`# Heading` → an `<h1>`). Runtime check: ZERO console/page errors.
 */
test("note editor loads a note, shows the toolbar, and Preview renders the heading — no console errors", async ({
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
  await page.goto("/notes/n1");

  // Title hydrated from the mocked get_note.
  await expect(page.locator(".note-title-input")).toHaveValue("My First Note");

  // Body textarea carries the front-matter-stripped body.
  const body = page.locator(".body-area");
  await expect(body).toHaveValue(/Some body text to select\./);

  // The formatting toolbar (H1/Bold/…) is present in edit mode.
  await expect(page.locator(".toolbar")).toBeVisible();
  await expect(page.locator(".tool", { hasText: "H1" }).first()).toBeVisible();

  // The Edit/Preview segmented toggle.
  const previewBtn = page.getByRole("button", { name: "Preview", exact: true });
  await expect(previewBtn).toBeVisible();

  // Toggle to Preview → the markdown "# Heading" renders as an <h1>.
  await previewBtn.click();
  await expect(page.locator(".note-preview")).toBeVisible();
  await expect(page.locator(".note-preview h1")).toHaveText("Heading");

  expect(consoleErrors).toEqual([]);
});

/**
 * A sealed-not-unlocked note (`get_note` returns the masked shape) renders the
 * lock gate instead of the body — no title/body leak — with no console errors.
 */
test("a locked note renders the lock gate (no body) with no console errors", async ({
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
  await page.goto("/notes/nlk");

  // The lock gate copy is present; the body textarea is NOT rendered.
  await expect(page.getByText(/locked folder/i)).toBeVisible();
  await expect(page.locator(".body-area")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});
