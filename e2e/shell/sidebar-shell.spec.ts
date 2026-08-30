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

test("opens expanded, with search on top and no brand mark", async ({ page }) => {
  await boot(page);
  const sb = sidebar(page);

  await expect(sb).toBeVisible();
  await expect(sb.getByRole("button", { name: "Search" })).toBeVisible();
  await expect(sb.getByRole("button", { name: "Collapse sidebar" })).toBeVisible();

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

  await sb.getByRole("button", { name: "Collapse sidebar" }).click();

  await expect(sb.getByText("Capture", { exact: true })).toBeHidden();
  await expect(sb.getByRole("tree", { name: "Workspaces" })).toHaveCount(0);
  await expect(sb.getByRole("button", { name: "Expand sidebar" })).toBeVisible();

  const collapsedBox = await sb.boundingBox();
  expect(collapsedBox).not.toBeNull();
  expect(expandedBox).not.toBeNull();
  expect(collapsedBox!.width).toBeLessThan(expandedBox!.width - 100);

  await page.reload();
  await expect(
    sidebar(page).getByRole("button", { name: "Expand sidebar" }),
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
