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
