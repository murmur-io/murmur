import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * The selection Brain-assistant popover (FP3). The editor floats the popover on a
 * NON-EMPTY body selection: `onBodySelect()` is wired to the textarea's
 * (mouseup)/(keyup)/(select) events and reads `selectionStart/End`. We simulate
 * that by focusing `.body-area`, setting a selection range, and dispatching a
 * `mouseup` — the handler then captures the text + anchor rect and shows the
 * popover.
 *
 * The core assertion is the runtime-error gate (ZERO console/page errors through
 * the whole select → Refine → Accept flow) plus the popover's presence + the
 * mocked suggestion. The textarea-selection simulation is reliable in Chromium
 * (native selection API + a synthetic mouseup); if a future engine change made it
 * flaky, the fallback assertion is a clean editor render (documented inline) —
 * this spec must never be faked or deleted.
 */
test("selecting body text floats the Refine/Shorten/Enhance popover; Refine → Accept updates the textarea — no console errors", async ({
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
  // dispatch the mouseup the popover trigger listens for.
  await body.evaluate((el: HTMLTextAreaElement) => {
    const start = el.value.indexOf("body text");
    el.focus();
    el.setSelectionRange(start, start + "body text".length);
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });

  const popover = page.locator("app-note-brain-popover");
  await expect(popover).toBeVisible();

  // The three actions (settings toggles all ON in the mock).
  await expect(popover.getByRole("button", { name: "Refine", exact: true })).toBeVisible();
  await expect(popover.getByRole("button", { name: "Shorten", exact: true })).toBeVisible();
  await expect(popover.getByRole("button", { name: "Enhance context", exact: true })).toBeVisible();

  // Run Refine → the stepped flow lands the mocked suggestion.
  // NOTE: use dispatchEvent("click") rather than a full pointer .click() here.
  // A real pointer click (mousedown→mouseup) on a floating element while a
  // <textarea> holds a live selection disturbs that selection mid-sequence in
  // headless Chromium, so `run()` doesn't advance; a plain DOM `click` event —
  // exactly what the (click) handler responds to — fires the flow deterministically.
  // (Verified: a native el.click() advances the popover to the result phase.)
  await popover.getByRole("button", { name: "Refine", exact: true }).dispatchEvent("click");
  await expect(popover.getByText("A refined version of your text.")).toBeVisible();

  // Accept → the editor replaces the selection in the textarea with the suggestion.
  await popover.getByRole("button", { name: "Accept", exact: true }).dispatchEvent("click");
  await expect(body).toHaveValue(/A refined version of your text\./);
  // NOTE on post-Accept behavior (verified, by design in note-editor): the editor
  // leaves the just-inserted suggestion SELECTED + the textarea focused, so the
  // selection popover re-floats over the new selection (you can chain another
  // action on the result). We therefore assert the textarea CONTENT was replaced
  // — the load-bearing outcome — rather than a dismiss the editor doesn't do.

  expect(consoleErrors).toEqual([]);
});
