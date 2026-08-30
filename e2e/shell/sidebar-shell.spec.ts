import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const FOREST = [
  {
    id: "p-acme",
    name: "Acme",
    kind: "meeting",
    level: "project",
    emoji: "🟣",
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
];

/**
 * ONE sidebar: search and the collapse control on top, destinations and the
 * Browse group under them, the workspace tree in the middle, the create actions
 * pinned to the bottom. There is no separate icon rail and no second contextual
 * panel any more — collapsing this sidebar IS the old rail.
 *
 * Row labels are plain text rather than accessible names: every control also
 * carries an `aria-label`, so `getByRole` sees the label and these assertions
 * deliberately go through `.sb-label` visibility to prove the COLLAPSED state
 * really removes the text.
 */
async function boot(page: Page): Promise<void> {
  await mockTauri(page, {}, { list_workspace_tree: FOREST });
  await page.goto("/record");
}

function sidebar(page: Page) {
  return page.getByRole("navigation", { name: "Primary navigation" });
}

function topbar(page: Page) {
  return page.locator("header.shell-topbar");
}

test("opens expanded, with search on top and no brand mark", async ({ page }) => {
  await boot(page);
  const sb = sidebar(page);

  await expect(sb).toBeVisible();

  // Search, Collapse and Settings share ONE band that sits ABOVE the sidebar,
  // in shell chrome rather than inside the panel's glass.
  const bar = topbar(page);
  const search = bar.getByRole("button", { name: "Search" });
  const collapse = bar.getByRole("button", { name: "Collapse sidebar" });
  const settings = bar.getByRole("link", { name: "Settings" });
  await expect(sb.getByRole("button", { name: "Search" })).toHaveCount(0);

  // Search is the icon alone — no label, no shortcut badge.
  await expect(bar.getByText("Search", { exact: true })).toHaveCount(0);
  await expect(search.locator("mur-icon")).toHaveAttribute("data-icon", "search");
  await expect(search).toBeVisible();
  await expect(collapse).toBeVisible();
  await expect(settings).toBeVisible();

  const boxes = await Promise.all([
    search.boundingBox(),
    collapse.boundingBox(),
    settings.boundingBox(),
  ]);
  const centres = boxes.map((b) => {
    expect(b).not.toBeNull();
    return b!.y + b!.height / 2;
  });
  // Same row: every centre within a pixel of the first.
  expect(centres[1]).toBeCloseTo(centres[0], 0);
  expect(centres[2]).toBeCloseTo(centres[0], 0);

  // And that row is level with the traffic lights, whose centre the shell puts
  // 36px below the window top (trafficLightPosition y:30 + a 12px button).
  expect(centres[0]).toBeGreaterThan(28);
  expect(centres[0]).toBeLessThan(44);

  // It must also clear them horizontally: the buttons end 84px in.
  const searchBox = await search.boundingBox();
  expect(searchBox).not.toBeNull();
  expect(searchBox!.x).toBeGreaterThanOrEqual(84);

  // The band is above the sidebar, not inside it.
  const sbBox = await sb.boundingBox();
  expect(sbBox).not.toBeNull();
  expect(centres[0]).toBeLessThan(sbBox!.y);

  // The Murmur logo tile is gone — it was chrome that navigated nowhere.
  await expect(sb.locator(".rail-brand")).toHaveCount(0);
  await expect(sb.locator('mur-icon[data-icon="murmur"]')).toHaveCount(0);

  // Destinations and the workspace tree share this one surface.
  await expect(sb.getByText("Capture", { exact: true })).toBeVisible();
  await expect(sb.getByText("Ask", { exact: true })).toBeVisible();
  await expect(sb.getByRole("tree", { name: "Workspaces" })).toBeVisible();
});

test("collapsing hides the labels and the tree, and survives a reload", async ({
  page,
}) => {
  await boot(page);
  const sb = sidebar(page);
  const expandedBox = await sb.boundingBox();

  await topbar(page).getByRole("button", { name: "Collapse sidebar" }).click();

  await expect(sb.getByText("Capture", { exact: true })).toBeHidden();
  await expect(sb.getByRole("tree", { name: "Workspaces" })).toHaveCount(0);
  await expect(sb.getByRole("button", { name: "Expand sidebar" })).toBeVisible();

  const collapsedBox = await sb.boundingBox();
  expect(collapsedBox).not.toBeNull();
  expect(expandedBox).not.toBeNull();
  expect(collapsedBox!.width).toBeLessThan(expandedBox!.width - 100);

  await page.reload();
  await expect(
    topbar(page).getByRole("button", { name: "Expand sidebar" }),
  ).toBeVisible();
});

test("Browse expands in place instead of opening a second panel", async ({
  page,
}) => {
  await boot(page);
  const sb = sidebar(page);

  await expect(sb.getByText("Meetings", { exact: true })).toHaveCount(0);
  await sb.getByRole("button", { name: "Browse" }).click();

  await expect(sb.getByText("Meetings", { exact: true })).toBeVisible();
  await expect(sb.getByText("Notes", { exact: true })).toBeVisible();
  // The destinations land INSIDE the one sidebar; no complementary panel opens.
  await expect(page.getByRole("complementary", { name: "Browse sidebar" })).toHaveCount(0);
});

test("the footer offers New note plus a menu for the other create actions", async ({
  page,
}) => {
  await boot(page);
  const sb = sidebar(page);

  await expect(sb.getByRole("button", { name: "New note" })).toBeVisible();

  await sb.getByRole("button", { name: "More create options" }).click();
  const menu = page.getByRole("menu");
  await expect(menu.getByRole("menuitem", { name: "New capture" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "New dashboard" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "New reminder" })).toBeVisible();

  await menu.getByRole("menuitem", { name: "New capture" }).click();
  await expect(page).toHaveURL(/\/record$/);
  await expect(page.getByRole("menu")).toHaveCount(0);
});

/**
 * REGRESSION (reported from a real window): with Browse expanded the sidebar
 * column grew past the viewport, so "New note" and "Settings" were pushed off
 * the bottom and overlapped the Workspaces section, with nothing to scroll.
 *
 * The footer is pinned chrome — it must stay reachable no matter how much the
 * middle holds — and the middle must scroll rather than push.
 */
test("the footer stays visible and the middle scrolls when Browse is expanded", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1200, height: 620 });
  await boot(page);
  const sb = sidebar(page);

  await sb.getByRole("button", { name: "Browse" }).click();
  await expect(sb.getByText("People", { exact: true })).toBeVisible();

  // New note is pinned in the footer; Settings sits in the band above and is
  // unaffected by the sidebar's own overflow.
  const newNote = sb.getByRole("button", { name: "New note" });
  const settings = topbar(page).getByRole("link", { name: "Settings" });
  await expect(newNote).toBeVisible();
  await expect(settings).toBeVisible();

  const sbBox = await sb.boundingBox();
  const noteBox = await newNote.boundingBox();
  const settingsBox = await settings.boundingBox();
  expect(sbBox).not.toBeNull();
  expect(noteBox).not.toBeNull();
  expect(settingsBox).not.toBeNull();
  const sidebarBottom = sbBox!.y + sbBox!.height;
  expect(noteBox!.y + noteBox!.height).toBeLessThanOrEqual(sidebarBottom + 1);

  // The overflow lives in ONE scroll region, not in the sidebar itself.
  const scroll = sb.locator(".sb-scroll");
  const overflows = await scroll.evaluate(
    (el) => el.scrollHeight > el.clientHeight,
  );
  expect(overflows).toBe(true);

  await scroll.evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });
  expect(await scroll.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);

  // Scrolling the middle moves neither the pinned footer nor the top band.
  const noteAfter = await newNote.boundingBox();
  const settingsAfter = await settings.boundingBox();
  expect(noteAfter!.y).toBeCloseTo(noteBox!.y, 0);
  expect(settingsAfter!.y).toBeCloseTo(settingsBox!.y, 0);
});
