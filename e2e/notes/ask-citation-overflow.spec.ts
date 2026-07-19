import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * VERIFICATION (2026-07-19) — the Ask-panel citation-pill overflow fix. A long
 * `[[Title]]` in an assistant answer must WRAP INSIDE the fixed-width Ask drawer
 * (`clamp(320,30vw,400)px`), not bleed past its right edge. Drives the note-chat
 * drawer with a mocked `ask_vault` that returns a very long wikilink title, then
 * asserts the rendered `.md-wikilink` chip's right edge stays within the pane.
 */
test("a long Ask citation wraps INSIDE the drawer pane (no horizontal overflow)", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") consoleErrors.push(m.text());
  });
  page.on("pageerror", (e) => consoleErrors.push(String(e)));

  await mockNotes(page, {
    ask_vault: () => ({
      answer:
        "Based on the note, here is the source: [[Test nagrania — prośba o analizę pogody na następny tydzień]] — it covers everything.",
      citations: [],
      sources: [],
      threadId: "t1",
      status: "ok",
    }),
  });

  await page.goto("/notes/n1");
  await expect(page.locator(".note-title-input")).toHaveValue("My First Note");

  // Open the Ask Brain drawer.
  await page.locator(".head-chat-btn").click();
  const drawer = page.locator(".note-chat-drawer");
  await expect(drawer).toBeVisible();

  // Ask a question → the mocked answer renders with the long citation pill.
  const input = drawer.locator(".chat-input");
  await input.fill("What is the source?");
  await input.press("Enter");

  const chip = drawer.locator(".md-wikilink");
  await expect(chip).toBeVisible();
  await expect(chip).toContainText("Test nagrania");

  // The chip's right edge must NOT exceed the drawer pane's right edge (overflow).
  const rects = await page.evaluate(() => {
    const chipEl = document.querySelector(".note-chat-drawer .md-wikilink")!;
    const bubble = chipEl.closest(".chat-bubble")!;
    const pane = document.querySelector(".note-chat-drawer")!;
    const c = chipEl.getBoundingClientRect();
    const bub = bubble.getBoundingClientRect();
    const p = pane.getBoundingClientRect();
    return {
      chipRight: Math.round(c.right),
      bubbleRight: Math.round(bub.right),
      paneRight: Math.round(p.right),
      chipHeight: Math.round(c.height),
      lineHeight: parseFloat(getComputedStyle(chipEl).fontSize),
    };
  });
  // Chip stays within the bubble, and the bubble within the pane (a few px slack).
  expect(rects.chipRight).toBeLessThanOrEqual(rects.paneRight + 1);
  expect(rects.bubbleRight).toBeLessThanOrEqual(rects.paneRight + 1);
  // It WRAPPED onto multiple lines (a single-line nowrap chip would be one
  // font-size-tall row; a wrapped one is clearly taller).
  expect(rects.chipHeight).toBeGreaterThan(rects.lineHeight * 1.5);

  await drawer.screenshot({
    path: "/private/tmp/claude-501/-Users-jakubgawronski-Projects-meetnotes/d3db29b4-fbd3-49ac-868a-fd86a0c14a1f/scratchpad/ask-citation.png",
  });

  expect(consoleErrors).toEqual([]);
});
