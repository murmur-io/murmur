import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Fix 1 — a rendered `[[Wikilink]]` pill in Preview mode opens the resolved
 * target through `TabsService` (a TRACKED TAB), not a raw `router.navigate` that
 * looked like a no-op / left an orphaned untracked view (the same sibling-function
 * bug already fixed for `NoteBrainPopoverComponent.openCitation`).
 */
test("clicking a rendered [[wikilink]] pill in Preview opens the target as a tracked tab", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, {
    get_note: (args: { id: string }) => ({
      id: args.id,
      title: args.id === "n1" ? "My First Note" : "Weekly plan",
      folderId: "nf1",
      markdown: args.id === "n1" ? "See [[Weekly plan]] for details." : "The plan body.",
      tags: [],
      properties: {},
      updatedAt: 1_720_000_000_000,
      createdAt: 1_719_000_000_000,
      exportedPath: null,
      locked: false,
      shared: false,
    }),
    resolve_wikilink: (args: { title: string }) =>
      args.title === "Weekly plan" ? { kind: "note", id: "n2" } : null,
  });
  // Open the FIRST note through a real in-app action (the Notes list row click →
  // `TabsService.openNote`) — a `page.goto` deep-link deliberately does NOT open a
  // tab (matches real usage: only an in-app open action creates one), so opening
  // via the list is what puts a first tab on the strip to compare against.
  await page.goto("/notes");
  await page.getByRole("button", { name: "My First Note" }).click();
  await expect(page).toHaveURL(/\/notes\/n1$/);
  await expect(page.locator(".tab-strip .tab-item")).toHaveCount(1);

  // Switch to Preview to render the wikilink pill.
  await page.getByRole("button", { name: "Preview", exact: true }).click();
  const pill = page.locator(".md-wikilink", { hasText: "Weekly plan" });
  await expect(pill).toBeVisible();
  await pill.click();

  // Navigated to the resolved note AND registered as a SECOND tracked tab (the
  // regression this fix closes — a raw router.navigate never added a tab).
  await expect(page).toHaveURL(/\/notes\/n2$/);
  await expect(page.locator(".tab-strip .tab-item")).toHaveCount(2);

  expect(consoleErrors).toEqual([]);
});

/**
 * Fix 2 — the slash-menu "Link to note" entry opens the SAME inline autocomplete
 * as the raw `[[` keystroke trigger; picking a candidate inserts `[[Title]]` at
 * the trigger position. Verifies BOTH trigger paths share one popover/codepath.
 */
test("slash menu 'Link to note' opens the autocomplete popover and inserts [[Title]] on pick", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, {
    list_link_candidates: () => [
      { kind: "note", id: "n2", title: "Weekly plan", snippet: "" },
      { kind: "meeting", id: "m1", title: "Kickoff", snippet: "" },
    ],
  });
  await page.goto("/notes/n1");

  const body = page.locator(".body-area");
  await expect(body).toBeVisible();

  // Move the caret to the end of a blank new line, then type `/`.
  await body.click();
  await body.evaluate((el: HTMLTextAreaElement) => {
    el.setSelectionRange(el.value.length, el.value.length);
  });
  await body.press("Enter");
  await body.press("/");

  const slashMenu = page.locator(".slash-menu");
  await expect(slashMenu).toBeVisible();
  await slashMenu.getByRole("option", { name: "Link to note" }).click();

  // The link picker is now open (OPAQUE overlay, T3) with the mocked candidates.
  const picker = page.locator("app-link-picker");
  await expect(picker).toBeVisible();
  await expect(picker.getByText("Weekly plan")).toBeVisible();
  await expect(picker.getByText("Kickoff")).toBeVisible();

  await picker.getByText("Weekly plan").click();

  // The picker closes and the body now carries the wikilink (replacing the `[[` trigger).
  await expect(picker).toHaveCount(0);
  await expect(body).toHaveValue(/\[\[Weekly plan\]\]/);

  expect(consoleErrors).toEqual([]);
});

/**
 * Fix 2 (parity) — typing the raw `[[` keystroke ALSO opens the picker (not just
 * the slash-menu entry), confirming the ONE shared trigger/component contract.
 */
test("typing [[ also opens the link-picker autocomplete", async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, {
    list_link_candidates: () => [
      { kind: "note", id: "n2", title: "Weekly plan", snippet: "" },
    ],
  });
  await page.goto("/notes/n1");

  const body = page.locator(".body-area");
  await expect(body).toBeVisible();
  await body.click();
  await body.evaluate((el: HTMLTextAreaElement) => {
    el.setSelectionRange(el.value.length, el.value.length);
  });
  await body.press("[");
  await body.press("[");

  const picker = page.locator("app-link-picker");
  await expect(picker).toBeVisible();
  await expect(picker.getByText("Weekly plan")).toBeVisible();

  expect(consoleErrors).toEqual([]);
});
