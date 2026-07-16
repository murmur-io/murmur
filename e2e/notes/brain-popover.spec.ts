import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * The selection Brain-assistant popover (FP3). Selecting body text now floats a
 * FORMATTING BUBBLE (`app-note-selection-toolbar`); the Brain popover opens only
 * when its "Ask Brain" button is pressed — the modal no longer auto-appears.
 * `onBodySelect()` is wired to the textarea's (mouseup)/(keyup)/(select) events
 * and reads `selectionStart/End`. We simulate that by focusing `.body-area`,
 * setting a selection range, and dispatching a `mouseup` — the handler captures
 * the text + anchor rect and floats the bubble; clicking "Ask Brain" mounts the
 * popover over that same selection.
 *
 * The core assertion is the runtime-error gate (ZERO console/page errors through
 * the whole select → Ask Brain → Refine → Accept flow) plus the popover's presence
 * + the mocked suggestion. The textarea-selection simulation is reliable in
 * Chromium (native selection API + a synthetic mouseup); this spec must never be
 * faked or deleted.
 */
test("selecting body text floats the bubble; Ask Brain → Refine → Accept updates the textarea — no console errors", async ({
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

  const body = page.locator(".body-area");
  await expect(body).toBeVisible();
  await expect(body).toHaveValue(/Some body text to select\./);

  // Simulate a real body selection: focus, select the "body text" substring, then
  // dispatch the mouseup the selection trigger listens for.
  await body.evaluate((el: HTMLTextAreaElement) => {
    const start = el.value.indexOf("body text");
    el.focus();
    el.setSelectionRange(start, start + "body text".length);
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });

  // The FORMATTING BUBBLE floats first (not the Brain modal). Its "Ask Brain"
  // button opens the popover. Use dispatchEvent("click") — a plain DOM click that
  // doesn't disturb the live textarea selection (see the Refine note below).
  const bubble = page.locator("app-note-selection-toolbar");
  await expect(bubble).toBeVisible();
  await expect(page.locator("app-note-brain-popover")).toHaveCount(0);
  await bubble.getByRole("button", { name: "Ask Brain" }).dispatchEvent("click");

  const popover = page.locator("app-note-brain-popover");
  await expect(popover).toBeVisible();

  // The ClickUp-style command menu: the "Ask Brain to edit…" input + the 5 quick
  // actions. Row buttons carry a label AND a description, so their accessible name
  // is "<label> <desc>" — match by (non-exact) label substring, not exact. "Enhance
  // context" is NOT a quick action anymore; it lives under "More actions".
  await expect(popover.getByPlaceholder("Ask Brain to edit…")).toBeVisible();
  await expect(popover.getByRole("button", { name: "Refine" })).toBeVisible();
  await expect(popover.getByRole("button", { name: "Shorten" })).toBeVisible();
  await expect(popover.getByRole("button", { name: "Find related" })).toBeVisible();
  await expect(popover.getByRole("button", { name: "Enhance context" })).toHaveCount(0);

  // Run Refine → the stepped flow lands the mocked replace suggestion.
  // NOTE: use dispatchEvent("click") rather than a full pointer .click() here.
  // A real pointer click (mousedown→mouseup) on a floating element while a
  // <textarea> holds a live selection disturbs that selection mid-sequence in
  // headless Chromium, so `run()` doesn't advance; a plain DOM `click` event —
  // exactly what the (click) handler responds to — fires the flow deterministically.
  // (Verified: a native el.click() advances the popover to the result phase.)
  await popover.getByRole("button", { name: "Refine" }).dispatchEvent("click");
  await expect(popover.getByText("A refined version of your text.")).toBeVisible();

  // Accept → the editor replaces the selection in the textarea with the suggestion
  // and clears the selection state, so both the popover and the bubble dismiss.
  await popover.getByRole("button", { name: "Accept", exact: true }).dispatchEvent("click");
  await expect(body).toHaveValue(/A refined version of your text\./);
  await expect(popover).toHaveCount(0);
  await expect(bubble).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});

/**
 * A3 — `find_related` gains an org citation (`kind: "org"`, backed by
 * `gather_note_enhance_citations`' new org-brain leg). Clicking it MUST route to
 * the read-only `/org-item/:id` viewer (mirrors `library.component.ts`'s
 * `orgItemLink`) — NOT the broken `/notes/<id>` fallback an unrecognized `kind`
 * used to fall into. This is the regression test for BOTH the new org leg AND the
 * incidental pre-existing `openCitation()` bug it made reachable.
 */
test("Find related: an org-kind citation routes to the read-only /org-item viewer, not /notes", async ({
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
    note_assistant_action: (args: { req: { action: string } }) => ({
      action: args.req.action,
      shape: "info",
      title: null,
      suggestion: "2 related sources in your brain.",
      citations: [
        {
          kind: "org",
          id: "oi-launchcode",
          title: "Anna's launchcode notes",
          snippet: "the launchcode rollout plan",
        },
        {
          kind: "note",
          id: "n2",
          title: "Weekly plan",
          snippet: "Ship the notes feature",
        },
      ],
      modelLabel: "Your brain (local search)",
      mode: "local",
      redacted: false,
    }),
    org_get_item: (args: { itemId: string }) => ({
      itemId: args.itemId,
      authorHint: "anna",
      title: "Anna's launchcode notes",
      createdAt: "2026-07-10T09:00:00Z",
      rev: 1,
      markdown: "# Anna's launchcode notes\n\nthe launchcode rollout plan",
    }),
  });
  await page.goto("/notes/n1");

  const body = page.locator(".body-area");
  await expect(body).toBeVisible();

  await body.evaluate((el: HTMLTextAreaElement) => {
    const start = el.value.indexOf("body text");
    el.focus();
    el.setSelectionRange(start, start + "body text".length);
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });

  const bubble = page.locator("app-note-selection-toolbar");
  await expect(bubble).toBeVisible();
  await bubble.getByRole("button", { name: "Ask Brain" }).dispatchEvent("click");

  const popover = page.locator("app-note-brain-popover");
  await expect(popover).toBeVisible();
  await popover.getByRole("button", { name: "Find related" }).dispatchEvent("click");

  // Minimalist reshape (2026-07-16): the count lead-line is gone — the rows ARE the
  // result. Each row's primary action inserts a link (accessible name "Insert link
  // to <title>"); "Open" stays as the quiet secondary action.
  const orgRow = popover.locator(".pop-cite-row", { hasText: "Anna's launchcode notes" });
  await expect(orgRow).toBeVisible();
  await expect(orgRow.locator(".pop-cite-kind")).toHaveText("org");

  await orgRow.getByRole("button", { name: "Open" }).dispatchEvent("click");

  // The old bug: an unrecognized `kind` fell through to `/notes/<id>` — assert the
  // FIX routes to the org-item viewer instead.
  await expect(page).toHaveURL(/\/org-item\/oi-launchcode$/);
  await expect(page).not.toHaveURL(/\/notes\/oi-launchcode$/);
  await expect(page.locator("app-org-item-viewer")).toBeVisible();

  expect(consoleErrors).toEqual([]);
});

/**
 * Fix 3 — "Insert link" on a `find_related` citation row inserts a `[[Title]]`
 * wikilink into the editor body (via the SAME wikilink-text builder the link
 * picker/toolbar op uses) instead of navigating away, and "Open" is still present
 * as the secondary action.
 */
test("Find related: Insert link drops a [[Title]] wikilink into the body instead of navigating", async ({
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
    note_assistant_action: (args: { req: { action: string } }) => ({
      action: args.req.action,
      shape: "info",
      title: null,
      suggestion: "1 related source in your brain.",
      citations: [
        {
          kind: "note",
          id: "n2",
          title: "Weekly plan",
          snippet: "Ship the notes feature",
        },
      ],
      modelLabel: "Your brain (local search)",
      mode: "local",
      redacted: false,
    }),
  });
  await page.goto("/notes/n1");

  const body = page.locator(".body-area");
  await expect(body).toBeVisible();

  await body.evaluate((el: HTMLTextAreaElement) => {
    const start = el.value.indexOf("body text");
    el.focus();
    el.setSelectionRange(start, start + "body text".length);
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });

  const bubble = page.locator("app-note-selection-toolbar");
  await expect(bubble).toBeVisible();
  await bubble.getByRole("button", { name: "Ask Brain" }).dispatchEvent("click");

  const popover = page.locator("app-note-brain-popover");
  await expect(popover).toBeVisible();
  await popover.getByRole("button", { name: "Find related" }).dispatchEvent("click");

  // Minimalist reshape (2026-07-16): no count lead-line — wait on the row itself.
  const row = popover.locator(".pop-cite-row", { hasText: "Weekly plan" });
  await expect(row).toBeVisible();
  // Both actions are present — Insert link (the row's primary action, accessible
  // name "Insert link to Weekly plan") AND Open (secondary), never removing the
  // existing capability.
  await expect(row.getByRole("button", { name: "Insert link" })).toBeVisible();
  await expect(row.getByRole("button", { name: "Open" })).toBeVisible();

  await row.getByRole("button", { name: "Insert link" }).dispatchEvent("click");

  // The popover dismisses and the editor body now carries the wikilink — no navigation.
  await expect(popover).toHaveCount(0);
  await expect(body).toHaveValue(/\[\[Weekly plan\]\]/);
  await expect(page).toHaveURL(/\/notes\/n1$/);

  expect(consoleErrors).toEqual([]);
});
