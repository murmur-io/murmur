import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * The note editor — loads a note via `get_note` and renders the title, body, and
 * Edit/Preview toggle. Formatting now lives in a floating bubble that appears on a
 * body selection (no persistent toolbar). Toggling to Preview renders the markdown
 * (`# Heading` → an `<h1>`). Runtime check: ZERO console/page errors.
 */
test("note editor loads a note, floats the formatting bubble on selection, and Preview renders the heading — no console errors", async ({
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

  // No persistent toolbar — formatting floats on a selection. Simulate one and
  // assert the bubble (H1/Bold/… + Ask Brain) appears above the selected text.
  await body.evaluate((el: HTMLTextAreaElement) => {
    const start = el.value.indexOf("body text");
    el.focus();
    el.setSelectionRange(start, start + "body text".length);
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });
  const bubble = page.locator(".sel-bar");
  await expect(bubble).toBeVisible();
  // The button label is its text ("H1"); the descriptive "Heading 1" is its title.
  await expect(bubble.getByRole("button", { name: "H1", exact: true })).toBeVisible();
  await expect(bubble.getByRole("button", { name: "Ask Brain" })).toBeVisible();

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
