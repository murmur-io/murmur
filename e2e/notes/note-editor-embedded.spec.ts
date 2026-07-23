import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * EMBEDDED-mode smoke (2026-07-17) — the shipped `NoteEditorComponent` gains an
 * additive `embedded` mode so the recording panel's "Note" tab can host the REAL
 * create-note editing experience on a companion note. This spec proves BOTH
 * halves of the contract:
 *
 *   (a) the ROUTED `/notes/:id` path is unchanged — header + title + properties
 *       still render (regression gate for `embedded()===false`);
 *   (b) a `<app-note-editor [embedded]="true" [noteIdInput]="'n1'">` mount shows
 *       ONLY the body editor + a working selection toolbar / Ask Brain popover —
 *       NO header, NO title input, NO properties bar, NO backlinks — and loads
 *       its note from `noteIdInput`, NOT the route.
 *
 * The recording panel is the shipped embedded host, so (b) starts a mocked
 * recording and exercises that REAL mount. This keeps the test on public app
 * behavior and avoids depending on Angular's optional `window.ng` debug exports,
 * whose private module shape differs between local and CI dev servers.
 */

test("(a) routed /notes/:id still renders header + title + properties (embedded=false unchanged)", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page);
  await page.goto("/notes/n1");

  // The full routed chrome is present.
  await expect(page.locator(".editor-head")).toBeVisible();
  await expect(page.locator(".note-title-input")).toHaveValue("My First Note");
  await expect(page.locator(".props")).toBeVisible();
  await expect(page.getByRole("button", { name: "Preview", exact: true })).toBeVisible();
  // Share is no longer a top-level header button (2026-07-19 header slim) — it lives
  // in the ⋯ menu now. Open ⋯ and confirm "Share…" is still reachable there.
  await expect(page.locator(".editor-head").getByRole("button", { name: "Share" })).toHaveCount(0);
  await page.getByRole("button", { name: "More actions" }).click();
  await expect(page.getByRole("menuitem", { name: /Share/ })).toBeVisible();

  // Not embedded → the section has no is-embedded class.
  await expect(page.locator("section.editor.is-embedded")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});

test("(b) embedded mount shows ONLY the body + working Ask Brain — no header/title/properties/backlinks", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, {
    model_present: () => true,
    start_recording: () => ({
      meetingId: "m-rec",
      startedAt: "2026-07-01T09:00:00Z",
    }),
    get_or_create_companion_note: () => ({
      noteId: "n1",
      meetingWikilink: "[[My First Note]]",
    }),
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  await expect(page.locator("button.stop-btn")).toBeVisible({ timeout: 10_000 });

  // Exercise the real shipped embedded host, including its signal inputs and DI.
  const embed = page.locator("app-meeting-conversation app-note-editor");
  await expect(embed).toBeVisible();
  await expect(embed.locator("section.editor.is-embedded")).toBeVisible();

  // Its body loads from noteIdInput (get_note('n1')) — the same body text.
  const body = embed.locator(".body-area");
  await expect(body).toBeVisible();
  await expect(body).toHaveValue(/Some body text to select\./);

  // CHROME IS GONE: no header, no title input, no properties bar, no relationships
  // panel (the merged "Related" app-connections is hidden when embedded — the
  // recording companion's Ask Brain tab owns relationships), no Preview/Share
  // toggle, and no Ask-about-this-note panel (source-scoped Brain PR-4).
  await expect(embed.locator(".editor-head")).toHaveCount(0);
  await expect(embed.locator(".note-title-input")).toHaveCount(0);
  await expect(embed.locator(".props")).toHaveCount(0);
  await expect(embed.locator("app-connections")).toHaveCount(0);
  await expect(embed.locator("app-note-chat")).toHaveCount(0);
  await expect(embed.getByRole("button", { name: "Preview", exact: true })).toHaveCount(0);

  // The IN-NOTE Ask Brain works embedded: select body text → the formatting
  // bubble floats → "Ask Brain" opens the brain popover over the SAME selection.
  await body.evaluate((el: HTMLTextAreaElement) => {
    const start = el.value.indexOf("body text");
    el.focus();
    el.setSelectionRange(start, start + "body text".length);
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });
  // The bubble + Brain popover boxes are TELEPORTED to <body> (appTeleportToBody),
  // so they're located by class at page level, not under the embed host.
  const bubble = page.locator(".sel-bar");
  await expect(bubble).toBeVisible();
  await bubble.getByRole("button", { name: "Ask Brain" }).dispatchEvent("click");
  await expect(page.locator(".brain-pop")).toBeVisible();

  expect(consoleErrors).toEqual([]);
});
