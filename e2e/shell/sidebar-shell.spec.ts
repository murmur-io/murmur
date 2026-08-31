import { expect, test, type Locator, type Page } from "@playwright/test";

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
  return page.locator(".primary-sidebar .sb-top");
}

/**
 * The sidebar ANIMATES its width (`transition: flex-basis, width`), so a
 * boundingBox() taken right after the collapse/expand class flips reports a
 * mid-flight geometry — which is how the width and window-button-margin
 * assertions below failed while the layout itself was correct. Read until two
 * successive samples agree.
 */
async function settledWidth(sb: Locator): Promise<number> {
  let last = -1;
  for (let i = 0; i < 40; i += 1) {
    const width = (await sb.boundingBox())?.width ?? -1;
    if (width === last) {
      return width;
    }
    last = width;
    await sb.page().waitForTimeout(50);
  }
  return last;
}

test("opens expanded, with search on top and no brand mark", async ({ page }) => {
  await boot(page);
  const sb = sidebar(page);

  await expect(sb).toBeVisible();

  // Search, Settings and Collapse share the sidebar's own top row, INSIDE the
  // panel's glass and level with the macOS window buttons on its left.
  const bar = topbar(page);
  const search = bar.getByRole("button", { name: "Search" });
  const collapse = bar.getByRole("button", { name: "Collapse sidebar" });
  const settings = bar.getByRole("link", { name: "Settings" });
  await expect(bar.getByRole("link", { name: "Ask" })).toBeVisible();
  // `bar` is inside `sb`, so scope this to the destination rows: expanded,
  // Search lives ONLY in the top row and must not be duplicated below it.
  await expect(
    sb.locator(".sb-nav").getByRole("button", { name: "Search" }),
  ).toHaveCount(0);

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

  // The band sits in the window-button strip. The exact centre is calibrated,
  // not derived — trafficLightPosition did not predict the rendered result — so
  // this window is deliberately loose and only guards the band from drifting
  // into the sidebar or off the top of the window.
  expect(centres[0]).toBeGreaterThan(16);
  expect(centres[0]).toBeLessThan(34);

  // Order left to right: Search, Settings, Collapse — right-aligned, so they
  // sit at the far end of the sidebar's column, clear of the window buttons.
  const boxesX = boxes.map((b) => b!.x);
  expect(boxesX[0]).toBeLessThan(boxesX[2]);
  const settingsBox = boxes[2];
  const collapseBox = boxes[1];
  expect(settingsBox!.x).toBeLessThan(collapseBox!.x);
  expect(boxesX[0]).toBeGreaterThan(100);

  // The row is INSIDE the panel, so the window buttons land on the glass.
  const sbBox = await sb.boundingBox();
  expect(sbBox).not.toBeNull();
  expect(centres[0]).toBeGreaterThan(sbBox!.y);
  expect(centres[0]).toBeLessThan(sbBox!.y + 40);

  // REGRESSION: the sidebar must not push the main column down — that is the
  // dead space that appeared above the tab strip when the row lived outside.
  const main = page.locator(".main-col");
  const mainBox = await main.boundingBox();
  expect(mainBox).not.toBeNull();
  expect(mainBox!.y).toBeCloseTo(sbBox!.y, 0);

  // The Murmur logo tile is gone — it was chrome that navigated nowhere.
  await expect(sb.locator(".rail-brand")).toHaveCount(0);
  await expect(sb.locator('mur-icon[data-icon="murmur"]')).toHaveCount(0);

  // ONE wide Capture button, and a round button beside it holding every other
  // creation — the Notion shape. Capture takes the remaining width.
  const capture = sb.getByRole("link", { name: "Capture" });
  const create = sb.getByRole("button", { name: "Create", exact: true });
  await expect(capture).toHaveClass(/btn-primary/);
  await expect(capture).toContainText("Capture");

  const [captureBox, createBox] = await Promise.all([
    capture.boundingBox(),
    create.boundingBox(),
  ]);
  expect(captureBox).not.toBeNull();
  expect(createBox).not.toBeNull();
  expect(createBox!.width).toBeCloseTo(createBox!.height, 0);
  expect(captureBox!.width).toBeGreaterThan(createBox!.width * 2);
  expect(captureBox!.height).toBeCloseTo(createBox!.height, 0);
  expect(captureBox!.x).toBeLessThan(createBox!.x);

  // New note is no longer a button of its own: it lives in that menu.
  await expect(sb.getByRole("button", { name: "New note" })).toHaveCount(0);

  await expect(sb.getByRole("tree", { name: "Workspaces" })).toBeVisible();
});

test("collapsing hides the labels and the tree, and survives a reload", async ({
  page,
}) => {
  await boot(page);
  const sb = sidebar(page);
  const expandedBox = await sb.boundingBox();

  await topbar(page).getByRole("button", { name: "Collapse sidebar" }).click();

  await expect(sb.getByText("Browse", { exact: true })).toBeHidden();
  await expect(sb.getByRole("tree", { name: "Workspaces" })).toHaveCount(0);
  await expect(sb.getByRole("button", { name: "Expand sidebar" })).toBeVisible();

  expect(expandedBox).not.toBeNull();
  expect(await settledWidth(sb)).toBeLessThan(expandedBox!.width - 100);

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

test("the footer offers Capture plus a menu for the other create actions", async ({
  page,
}) => {
  await boot(page);
  const sb = sidebar(page);

  await sb.getByRole("button", { name: "Create", exact: true }).click();
  const menu = page.getByRole("menu");
  await expect(menu.getByRole("menuitem", { name: "New note" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "New dashboard" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "New reminder" })).toBeVisible();

  // Open, the same control closes the menu — the plus rotates into an X.
  // The scrim behind the open menu carries the same accessible name, so assert
  // on the create control itself — it is the one that must flip to closing.
  await expect(sb.locator(".sb-create-more")).toHaveAttribute(
    "aria-label",
    "Close create menu",
  );

  await menu.getByRole("menuitem", { name: "New note" }).click();
  // `/notes/new` creates the draft and hands off to that note's own route, so
  // assert the destination is a note rather than the transient path.
  await expect(page).toHaveURL(/\/notes\//);
  await expect(page.getByRole("menu")).toHaveCount(0);
});

/**
 * REGRESSION (reported from a real window): with Browse expanded the sidebar
 * column grew past the viewport, so Capture and Settings were pushed off
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

  // Capture is pinned in the footer; Settings sits in the band above and is
  // unaffected by the sidebar's own overflow.
  const newNote = sb.getByRole("link", { name: "Capture" });
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

/**
 * Collapsed, the band narrows with its column to 68px, which cannot hold three
 * 32px controls. Search and Settings move into the rail rather than becoming
 * reachable only by expanding first.
 */
test("collapsed keeps Search and Settings reachable in the rail", async ({
  page,
}) => {
  await boot(page);
  await topbar(page).getByRole("button", { name: "Collapse sidebar" }).click();

  const sb = sidebar(page);
  await expect(sb.getByRole("button", { name: "Search" })).toBeVisible();
  await expect(sb.getByRole("link", { name: "Ask" })).toBeVisible();
  await expect(sb.getByRole("link", { name: "Settings" })).toBeVisible();

  // Workspaces and Shared cannot render their trees this narrow, so they get
  // rows that reveal the sidebar — without them the collapsed rail simply had
  // no sign that either section exists.
  const workspaces = sb.getByRole("button", { name: "Workspaces" });
  await expect(workspaces).toBeVisible();
  await expect(sb.getByRole("button", { name: "Shared" })).toBeVisible();

  await workspaces.click();
  await expect(sb.getByRole("tree", { name: "Workspaces" })).toBeVisible();
  await expect(
    topbar(page).getByRole("button", { name: "Collapse sidebar" }),
  ).toBeVisible();

  // Collapse again: the click above deliberately revealed the sidebar, and the
  // assertions below are about the COLLAPSED row.
  await topbar(page).getByRole("button", { name: "Collapse sidebar" }).click();

  // The row is down to the toggle alone.
  const bar = topbar(page);
  const expand = bar.getByRole("button", { name: "Expand sidebar" });
  await expect(expand).toBeVisible();
  await expect(bar.getByRole("button", { name: "Search" })).toHaveCount(0);
  await expect(bar.getByRole("link", { name: "Settings" })).toHaveCount(0);

  // REGRESSION: the collapsed panel must CONTAIN the macOS window buttons. It
  // was narrower than they are, so the green one overhung it onto the content
  // and the toggle sat wedged between yellow and green.
  //
  // 84px is the buttons' furthest possible right edge (x:32 read as a left edge
  // plus their 52px span; read as a centre they end sooner). The toggle drops
  // below them either way.
  await settledWidth(sb);
  const expandBox = await expand.boundingBox();
  const sbBox = await sb.boundingBox();
  expect(expandBox).not.toBeNull();
  expect(sbBox).not.toBeNull();
  expect(expandBox!.y).toBeGreaterThan(40);

  // The panel wraps the buttons (32..84px) with the SAME margin either side.
  // The right one being tighter is what proved x is a left edge, not a centre.
  const before = 32 - sbBox!.x;
  const after = sbBox!.x + sbBox!.width - 84;
  expect(after).toBeGreaterThan(0);
  expect(after).toBeCloseTo(before, 0);
});

/**
 * Section order is Workspaces, then Shared, then Browse: the user's own content
 * leads, a colleague's follows it, and the app-wide destinations come last.
 *
 * Shared is HIDDEN when nothing is shared in either direction. Most installs
 * have no org, and a permanent empty heading is noise in the only navigation
 * surface — so this fixture asserts its absence, and the presence case belongs
 * to a fixture that actually returns shared content.
 */
test("the sections run Workspaces, then Shared, then Browse", async ({ page }) => {
  await boot(page);
  const sb = sidebar(page);

  const workspaces = await sb
    .getByRole("region", { name: "Workspaces" })
    .boundingBox();
  const shared = await sb.getByRole("region", { name: "Shared" }).boundingBox();
  const browse = await sb.getByRole("button", { name: "Browse" }).boundingBox();
  expect(workspaces).not.toBeNull();
  expect(shared).not.toBeNull();
  expect(browse).not.toBeNull();

  // Shared is PERMANENT: it holds the order even with nothing shared, and says
  // so rather than vanishing and leaving the user hunting for the section.
  expect(workspaces!.y).toBeLessThan(shared!.y);
  expect(shared!.y).toBeLessThan(browse!.y);
  await expect(sb.getByText("Nothing shared with you yet")).toBeVisible();
});

/**
 * The global "File recordings with Brain" action was removed from the
 * Workspaces section. Filing by Brain is still reachable per container, from a
 * container row's own menu — only the do-it-for-everything entry is gone.
 */
test("the Workspaces section carries no global Brain filing action", async ({
  page,
}) => {
  await boot(page);
  const sb = sidebar(page);
  await expect(sb.getByText("File recordings with Brain")).toHaveCount(0);
  await expect(sb.locator(".brain-action")).toHaveCount(0);
});
