import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * The "Ask about this note" chat (`app-note-chat`) was RE-HOMED (2026-07-17) from
 * below the note body (off-screen, below the fold) into a RIGHT-SIDE COLLAPSIBLE
 * DRAWER toggled by an "Ask Brain" header button. This spec drives that layout,
 * with a mocked Tauri IPC + a ZERO console/page-error gate:
 *  - default COLLAPSED (no drawer, no `app-note-chat` in the DOM);
 *  - the header "Ask Brain" toggle OPENS the drawer, mounting `app-note-chat`
 *    inside `.note-chat-drawer`, docked to the RIGHT (not below the fold);
 *  - the drawer's close `×` (and the header toggle again) CLOSES it;
 *  - the open state PERSISTS across a reload (localStorage);
 *  - the toggle + drawer are ABSENT for a locked note.
 */

test("Ask Brain header toggle opens/closes the right drawer with note-chat inside; persists across reload — no console errors", async ({
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

  // Editor loaded.
  await expect(page.locator(".note-title-input")).toHaveValue("My First Note");

  // Default COLLAPSED — the header toggle is present but the drawer + chat are not.
  const toggle = page.getByRole("button", { name: "Ask Brain" }).and(
    page.locator(".head-chat-btn"),
  );
  await expect(toggle).toBeVisible();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".note-chat-drawer")).toHaveCount(0);
  await expect(page.locator("app-note-chat")).toHaveCount(0);

  // OPEN — the drawer docks on the right with the chat inside it.
  await toggle.click();
  const drawer = page.locator(".note-chat-drawer");
  await expect(drawer).toBeVisible();
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(drawer.locator("app-note-chat")).toBeVisible();
  // The chat's own "Ask about this note" heading IS the panel title now (the redundant drawer
  // header row was removed so the drawer reads as one coherent surface).
  await expect(
    drawer.getByRole("heading", { name: "Ask about this note" }),
  ).toBeVisible();

  // TWO SEPARATE COLUMNS (2026-07-19): the drawer is an IN-FLOW second column beside the
  // document, NOT a floating overlay — so opening it SHRINKS `.editor-body` instead of covering
  // the note text. Assert the two containers are ADJACENT and NON-OVERLAPPING (the drawer's left
  // edge sits at the body's right edge) and that together they fill the row. This is the
  // regression guard for the reported bug: `position: fixed` floated the rail OVER the note; the
  // fix put it back in normal flow so the note keeps its own space and the chat keeps its own.
  await page.waitForTimeout(500); // let the translateX slide-in settle before measuring resting rects
  const layout = await page.evaluate(() => {
    const d = document.querySelector(".note-chat-drawer")!.getBoundingClientRect();
    const b = document.querySelector(".editor-body")!.getBoundingClientRect();
    const main = document.querySelector(".editor-main")!.getBoundingClientRect();
    return {
      drawerLeft: Math.round(d.left),
      drawerRight: Math.round(d.right),
      drawerTop: Math.round(d.top),
      drawerBottom: Math.round(d.bottom),
      bodyLeft: Math.round(b.left),
      bodyRight: Math.round(b.right),
      mainRight: Math.round(main.right),
      viewportH: window.innerHeight,
    };
  });
  // Adjacent, not overlapping — the drawer begins exactly where the body ends (± a rounding px).
  expect(Math.abs(layout.drawerLeft - layout.bodyRight)).toBeLessThanOrEqual(1);
  // The drawer is to the RIGHT of the body, and the body still holds real width (never collapsed).
  expect(layout.drawerLeft).toBeGreaterThan(layout.bodyLeft);
  expect(layout.bodyRight).toBeGreaterThan(layout.bodyLeft + 100);
  // The pair fills the row: the drawer's right edge reaches the editor-main's right edge.
  expect(Math.abs(layout.drawerRight - layout.mainRight)).toBeLessThanOrEqual(1);
  // On-screen (not below the fold): its top is within the viewport.
  expect(layout.drawerTop).toBeLessThan(layout.viewportH);
  // REACHES THE BOTTOM (2026-07-19, user request): the pane runs to the very bottom of the window
  // — not cut off ~64px short by app-main's bottom padding. Guards a regression of the negative
  // `margin-bottom` / no-`space-8` host height that makes the split flush to the bottom edge.
  expect(Math.abs(layout.drawerBottom - layout.viewportH)).toBeLessThanOrEqual(4);

  // SPLIT VIEW: the two panes start at the SAME top (the drawer is a full-height COLUMN, not a bar
  // hanging under the header), so the note header's top aligns with the drawer pane's top. And the
  // two pane headers share ONE horizontal divider — the chat's own header band (`.chat-head`)
  // bottom lines up with the note header's bottom. This guards the "welded at the top, cut at the
  // bottom" regression: a continuous vertical divider needs both panes to begin at the same Y.
  const dock = await page.evaluate(() => {
    const head = document.querySelector(".editor-head")!.getBoundingClientRect();
    const d = document.querySelector(".note-chat-drawer")!.getBoundingClientRect();
    const chatHead = document
      .querySelector(".note-chat-drawer .chat-head")!
      .getBoundingClientRect();
    return {
      headTop: Math.round(head.top),
      headBottom: Math.round(head.bottom),
      drawerTop: Math.round(d.top),
      chatHeadBottom: Math.round(chatHead.bottom),
    };
  });
  // Both panes begin at the same Y (the drawer isn't offset below the header).
  expect(Math.abs(dock.drawerTop - dock.headTop)).toBeLessThanOrEqual(2);
  // The two header bands share one horizontal divider line (± a few px for sub-pixel rounding).
  expect(Math.abs(dock.chatHeadBottom - dock.headBottom)).toBeLessThanOrEqual(3);

  // CLOSE via the drawer's × — the drawer + chat leave the DOM.
  await drawer.getByRole("button", { name: "Close Ask Brain" }).click();
  await expect(page.locator(".note-chat-drawer")).toHaveCount(0);
  await expect(toggle).toHaveAttribute("aria-expanded", "false");

  // Re-open, then RELOAD → the open state persists (localStorage).
  await toggle.click();
  await expect(page.locator(".note-chat-drawer")).toBeVisible();
  await page.reload();
  await expect(page.locator(".note-title-input")).toHaveValue("My First Note");
  await expect(page.locator(".note-chat-drawer")).toBeVisible();
  await expect(page.locator(".note-chat-drawer app-note-chat")).toBeVisible();

  expect(consoleErrors).toEqual([]);
});

/**
 * A locked note shows NO Ask Brain toggle and NO drawer even if the persisted
 * preference is "open" — the lock gate replaces the whole document region, and the
 * header actions (including the toggle) are hidden for a locked note.
 */
test("a locked note shows no Ask Brain toggle and no drawer even when the pref is open — no console errors", async ({
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
  // Force the persisted preference OPEN before the app boots, to prove the lock
  // gate suppresses the drawer regardless of the stored flag.
  await page.addInitScript(() => {
    try {
      localStorage.setItem("murmur-note-chat-open", "1");
    } catch {
      /* ignore */
    }
  });
  await page.goto("/notes/nlk");

  // The lock gate is shown; neither the toggle nor the drawer is present.
  await expect(page.getByText(/locked folder/i)).toBeVisible();
  await expect(page.locator(".head-chat-btn")).toHaveCount(0);
  await expect(page.locator(".note-chat-drawer")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});
