import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * The meeting's four tool drawers are ONE choice, and the command row says so.
 *
 * Action items, Live context, Smart reminders and Ask were four independent
 * ghost toggles, but the shell holds a single `_openDrawer` signal
 * (`"ask" | "actions" | "smart" | "live" | null`) — at most one drawer is ever
 * open, and picking a second retires the first. Nothing in the UI said that
 * until you clicked and watched something else close, and nothing in the suite
 * asserted it either: the existing specs each open ONE drawer, so four toggles
 * that all opened at once would have kept every one of them green.
 *
 * These are the oracles for the segment that replaced them. The exclusivity
 * test is the one that could not fail before this change existed as a claim;
 * the naming test is what stops the regrouping from quietly renaming controls
 * that four other spec files locate by exact name.
 */
const TOOLS = ["Action items", "Live context", "Smart reminders", "Ask"] as const;

async function openMeeting(page: Page): Promise<void> {
  await mockTauri(page);
  await page.goto("/meeting/m-atlas-roadmap");
  await expect(tools(page)).toBeVisible();
}

/** The tool switcher, addressed the way a user would read it: one group. */
function tools(page: Page) {
  return page.getByRole("group", { name: "Meeting tools" });
}

function tool(page: Page, name: string) {
  return tools(page).getByRole("button", { name, exact: true });
}

/** Which tools report themselves as open. Exclusivity is a claim about ALL. */
async function expanded(page: Page): Promise<string[]> {
  const open: string[] = [];
  for (const name of TOOLS) {
    if ((await tool(page, name).getAttribute("aria-expanded")) === "true") {
      open.push(name);
    }
  }
  return open;
}

test("at most one tool drawer is open, and picking another retires it", async ({
  page,
}) => {
  await openMeeting(page);
  expect(await expanded(page)).toEqual([]);

  await tool(page, "Action items").click();
  await expect(page.locator(".actions-drawer")).toBeVisible();
  expect(await expanded(page)).toEqual(["Action items"]);

  await tool(page, "Live context").click();
  await expect(page.locator(".live-drawer")).toBeVisible();
  // The point of the test: the FIRST drawer is gone, and its control agrees.
  await expect(page.locator(".actions-drawer")).toHaveCount(0);
  expect(await expanded(page)).toEqual(["Live context"]);

  await tool(page, "Ask").click();
  await expect(page.locator(".ask-drawer")).toBeVisible();
  await expect(page.locator(".live-drawer")).toHaveCount(0);
  expect(await expanded(page)).toEqual(["Ask"]);
});

test("pressing the open tool closes it, leaving no drawer open", async ({
  page,
}) => {
  // "Nothing open" is a real state here, which is why these are toggles with
  // `aria-expanded` rather than tabs: a tablist cannot express it.
  await openMeeting(page);
  await tool(page, "Smart reminders").click();
  await expect(page.locator(".smart-drawer")).toBeVisible();

  await tool(page, "Smart reminders").click();
  await expect(page.locator(".smart-drawer")).toHaveCount(0);
  expect(await expanded(page)).toEqual([]);
});

test("Ask still focuses its composer when the segment opens it", async ({
  page,
}) => {
  // The segment dispatches to the per-drawer toggles instead of setting the
  // signal itself, precisely because opening is not uniform. A switcher that
  // took the shortcut would drop this focus and nothing else would notice.
  await openMeeting(page);
  await tool(page, "Ask").click();
  await expect(page.locator(".ask-drawer .chat-input")).toBeFocused();
});

test("every tool keeps the exact name other specs locate it by", async ({
  page,
}) => {
  await openMeeting(page);
  for (const name of TOOLS) {
    await expect(tool(page, name)).toBeVisible();
  }
  // Ask is the one tool whose label is PAINTED, not just announced: it is what
  // a user goes looking for by name, and an unlabelled sparkle is not findable.
  // `toContainText` reads rendered text, so it fails if Ask ever drops to an
  // aria-label-only icon — which `toBeVisible` above would happily accept.
  await expect(tool(page, "Ask")).toContainText("Ask");
  // The converse for an icon-only tool: its name must come from `aria-label`,
  // not from text this row has no space to paint.
  await expect(tool(page, "Action items")).toHaveAttribute(
    "aria-label",
    "Action items",
  );
});

test("the two switchers in the command row are the same control", async ({
  page,
}) => {
  // They sit one beside the other, so any divergence is visible at a glance —
  // which is why they share `.seg.seg--icon` instead of being two lookalikes.
  await openMeeting(page);
  const boxes = await page.evaluate(() => {
    const segs = [
      ...document.querySelectorAll<HTMLElement>(".head-actions .seg--icon"),
    ];
    return segs.map((seg) => {
      const segStyle = getComputedStyle(seg);
      const button = seg.querySelector<HTMLElement>(".seg-btn")!;
      const buttonStyle = getComputedStyle(button);
      return [
        segStyle.padding,
        segStyle.gap,
        segStyle.borderRadius,
        segStyle.backgroundColor,
        segStyle.borderColor,
        buttonStyle.height,
        buttonStyle.minWidth,
        buttonStyle.borderRadius,
      ].join(" | ");
    });
  });
  expect(boxes).toHaveLength(2);
  expect(boxes[0]).toBe(boxes[1]);
});
