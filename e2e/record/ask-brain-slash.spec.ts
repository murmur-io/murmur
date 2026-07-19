import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Calm-Notepad discoverability (2026-07-19): typing `/` in the recording note surfaces
 * "✦ Ask Brain" as the FIRST slash-menu item (Notion-style AI-at-the-top), and picking
 * it summons the Ask-Brain panel with the bare `/` stripped from the note. Guards the
 * fix for it being buried at the bottom of the scrollable block menu (undiscoverable).
 */
test("typing / in the recording note shows Ask Brain first and summons the panel", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") consoleErrors.push(m.text());
  });
  page.on("pageerror", (e) => consoleErrors.push(String(e)));

  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({
      meetingId: "m-rec",
      startedAt: "2026-07-01T09:00:00Z",
    }),
    get_or_create_companion_note: () => ({
      noteId: "n1",
      meetingWikilink: "[[Test Meeting]]",
    }),
    get_note: () => ({
      id: "n1",
      title: "Test Meeting",
      folderId: "",
      markdown: '---\nmeeting: "[[Test Meeting]]"\n---\n',
      tags: [],
      properties: {},
      updatedAt: 0,
      createdAt: 0,
      exportedPath: null,
      locked: false,
      shared: false,
    }),
    get_backlinks: () => [],
  });

  await page.goto("/record");
  await page.locator("button.start-btn").click();
  const body = page.locator(
    "app-meeting-conversation app-note-editor .editor-body textarea.body-area",
  );
  await expect(body).toBeVisible({ timeout: 10_000 });

  // Type "/" at line start → the slash menu opens with Ask Brain pinned FIRST.
  await body.click();
  await body.press("/");
  const menu = page.locator("app-meeting-conversation .slash-menu");
  await expect(menu).toBeVisible();
  await expect(menu.locator(".menu-item").first()).toHaveText(/Ask Brain/);
  await expect(menu.locator(".menu-item.is-ask")).toHaveText(/Ask Brain/);

  // Pick it → the Ask panel is summoned and the bare "/" is stripped from the note.
  await menu.locator(".menu-item.is-ask").click();
  await expect(
    page.locator("app-meeting-conversation .ask-panel"),
  ).toBeVisible();
  await expect(body).toHaveValue("");
  expect(consoleErrors).toEqual([]);
});
