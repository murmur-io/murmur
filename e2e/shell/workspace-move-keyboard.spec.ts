import { expect, test, type Page } from "@playwright/test";

import { mockDestinationPicker } from "../notes/destination-picker-mock";

import { mockTauri } from "../settings-ai/mock-invoke";

const FOREST = [
  {
    id: "p-acme",
    name: "Acme",
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [
      {
        kind: "meeting",
        total: 1,
        items: [
          { kind: "meeting", id: "m-1", title: "Standup", durationS: 900, sortAt: 2 },
        ],
      },
    ],
  },
  {
    id: "p-target",
    name: "Beta",
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [
      {
        id: "f-beta-shared",
        name: "Shared",
        kind: "meeting",
        level: "folder",
        emoji: null,
        tint: null,
        locked: false,
        unlocked: false,
        isRoot: false,
        folders: [],
        groups: [],
      },
      {
        id: "f-beta-fail",
        name: "Archive",
        kind: "meeting",
        level: "folder",
        emoji: null,
        tint: null,
        locked: false,
        unlocked: false,
        isRoot: false,
        folders: [],
        groups: [],
      },
      {
        id: "f-beta-archive-two",
        name: "Archive",
        kind: "meeting",
        level: "folder",
        emoji: null,
        tint: null,
        locked: false,
        unlocked: false,
        isRoot: false,
        folders: [],
        groups: [],
      },
    ],
    groups: [],
  },
  {
    id: "p-gamma",
    name: "Gamma",
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [
      {
        id: "f-gamma-shared",
        name: "Shared",
        kind: "meeting",
        level: "folder",
        emoji: null,
        tint: null,
        locked: false,
        unlocked: false,
        isRoot: false,
        folders: [],
        groups: [],
      },
    ],
    groups: [],
  },
  {
    id: "p-sealed",
    name: "Clients",
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    locked: true,
    unlocked: false,
    isRoot: false,
    folders: [
      {
        id: "f-stale-secret",
        name: "Secret child",
        kind: "meeting",
        level: "folder",
        emoji: null,
        tint: null,
        locked: false,
        unlocked: false,
        isRoot: false,
        folders: [],
        groups: [],
      },
    ],
    groups: [],
  },
];

async function open(page: Page): Promise<void> {
  await mockTauri(
    page,
    {
      move_container: (args: unknown) => {
        ((window as any).__containerMoves ??= []).push(args);
        return null;
      },
      move_note: (args: unknown) => {
        (globalThis as unknown as { __moves: unknown[] }).__moves ??= [];
        (globalThis as unknown as { __moves: unknown[] }).__moves.push(args);
        if ((args as { folderId?: string }).folderId === "f-beta-fail") {
          return Promise.reject(new Error("destination closed"));
        }
        return null;
      },
    },
    { list_workspace_tree: FOREST },
  );
  await mockDestinationPicker(page);
  await page.goto("/");
  await expect(page.getByRole("tree", { name: "Workspaces" })).toBeVisible();
  await page.getByRole("button", { name: "Expand Acme" }).click();
}

test("an item can be filed without a pointer", async ({ page }) => {
  await open(page);
  const trigger = page.getByRole("button", { name: "Actions for meeting Standup" });
  await trigger.focus();
  await page.keyboard.press("Enter");
  await page.getByRole("menuitem", { name: "Move…", exact: true }).focus();
  await page.keyboard.press("Enter");
  const sheet = page.getByRole("dialog", { name: "Move “Standup”" });
  await expect(sheet).toBeVisible();
  const here = sheet.locator('[data-row="c:p-acme"] .rhp-row-main');
  await expect(here).toContainText("Here");
  await here.focus();
  await here.press("ArrowLeft");
  await here.press("ArrowDown");
  const beta = sheet.locator('[data-row="c:p-target"] .rhp-row-main');
  await expect(beta).toBeFocused();
  await beta.press("ArrowRight");
  await beta.press("ArrowDown");
  const destination = sheet.locator('[data-row="c:f-beta-shared"] .rhp-row-main');
  await expect(destination).toBeFocused();
  await destination.press("Enter");
  await expect(destination).toHaveAttribute("aria-selected", "true");
  await destination.press("Control+Enter");
  await expect(sheet).toHaveCount(0);
  expect(await page.evaluate(() => (globalThis as any).__moves ?? [])).toEqual([
    { meetingId: "m-1", folderId: "f-beta-shared", confirmedEncryptionBoundary: false },
  ]);
});

test("the hierarchy searches full paths and retains selection after a failed write", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Actions for meeting Standup" }).click();
  await page.getByRole("menuitem", { name: "Move…", exact: true }).click();
  const sheet = page.getByRole("dialog", { name: "Move “Standup”" });
  await sheet.getByRole("searchbox", { name: "Search destinations" }).fill("Shared");
  await expect(sheet.getByText("Beta / Shared", { exact: true })).toBeVisible();
  await expect(sheet.getByText("Gamma / Shared", { exact: true })).toBeVisible();
  await sheet.getByRole("searchbox", { name: "Search destinations" }).fill("Gamma / Shared");
  await expect(sheet.locator('.rhp-label')).toHaveText(["Shared"]);
  await sheet.getByRole("searchbox", { name: "Search destinations" }).fill("Beta / Archive");
  await expect(sheet.locator('.rhp-label')).toHaveText(["Archive", "Archive"]);
  await sheet.locator('[data-row="h:container:f-beta-fail"] .rhp-row-main').click();
  await sheet.getByRole("button", { name: "Move here", exact: true }).click();
  await expect(sheet.getByRole("alert")).toContainText("destination closed");
  await expect(sheet.getByRole("button", { name: "Move here", exact: true })).toBeEnabled();
  await sheet.getByRole("searchbox", { name: "Search destinations" }).fill("Gamma / Shared");
  await sheet.locator('[data-row="h:container:f-gamma-shared"] .rhp-row-main').click();
  await sheet.getByRole("button", { name: "Move here", exact: true }).click();
  await expect(sheet).toHaveCount(0);
});

test("the picker exposes only sealed container names and restores focus on Escape", async ({ page }) => {
  await open(page);
  const trigger = page.getByRole("button", { name: "Actions for meeting Standup" });
  await trigger.click();
  await page.getByRole("menuitem", { name: "Move…", exact: true }).click();
  const sheet = page.getByRole("dialog", { name: "Move “Standup”" });
  await expect(sheet).toBeVisible();
  await expect(sheet).not.toContainText("Secret child");
  await expect(sheet.locator('[data-row="c:p-sealed"] .rhp-row-main')).toHaveAttribute("aria-disabled", "true");
  await page.keyboard.press("Escape");
  await expect(sheet).toHaveCount(0);
  await expect(trigger).toBeFocused();
});

test("Workspace containers use the same picker and reject self and descendants", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Expand Beta" }).click();
  await page.getByRole("button", { name: "Actions for Beta", exact: true }).click();
  await page.getByRole("menuitem", { name: "Move Workspace…", exact: true }).click();
  const sheet = page.getByRole("dialog", { name: "Move “Beta”" });
  await expect(sheet.locator('[data-row="c:p-target"] .rhp-row-main')).toContainText("Moving");
  await sheet.getByRole("button", { name: "Expand Beta" }).click();
  const descendant = sheet.locator('[data-row="c:f-beta-shared"] .rhp-row-main');
  await expect(descendant).toContainText("Inside what you're moving");
  await expect(descendant).toHaveAttribute("aria-disabled", "true");
  await descendant.focus();
  await descendant.press("Enter");
  await expect(sheet.getByRole("button", { name: "Move here", exact: true })).toBeDisabled();
  await sheet.getByRole("searchbox", { name: "Search destinations" }).fill("Gamma / Shared");
  await expect(sheet.getByText("Gamma / Shared", { exact: true })).toBeVisible();
  await sheet.locator('[data-row="h:container:f-gamma-shared"] .rhp-row-main').click();
  await sheet.getByRole("button", { name: "Move here", exact: true }).click();
  await expect(sheet).toHaveCount(0);
});

test("a folder in the Workspace tree moves through the searchable hierarchy", async ({page}) => {
  await open(page);
  await page.getByRole('button', {name:'Expand Beta', exact:true}).click();
  await page.getByRole('button', {name:'Actions for Shared', exact:true}).first().click();
  await page.getByRole('menuitem', {name:'Move folder…', exact:true}).click();
  const sheet = page.getByRole('dialog', {name:'Move “Shared”'});
  await expect(sheet.locator('[data-row="c:p-target"] .rhp-row-main')).toContainText('Here');
  await sheet.getByRole('searchbox', {name:'Search destinations'}).fill('Gamma / Shared');
  await expect(sheet.getByText('Gamma / Shared', {exact:true})).toBeVisible();
  await sheet.getByRole('button', {name:'Choose Shared', exact:true}).click();
  await sheet.getByRole('button', {name:'Move here', exact:true}).click();
  await expect(sheet).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).__containerMoves)).toEqual([
    {id:'f-beta-shared', parentId:'f-gamma-shared'},
  ]);
});

test('a session-unlocked encrypted Workspace cannot be moved out', async ({page}) => {
  await mockTauri(page, {}, {list_workspace_tree:[{
    ...FOREST[0], name:'Session private', locked:true, unlocked:true,
  }]});
  await page.goto('/');
  await page.getByRole('button', {name:'Actions for Session private',exact:true}).click();
  const move = page.getByRole('menuitem', {name:'Move Workspace…',exact:true});
  await expect(move).toBeDisabled();
  await expect(move).toHaveAttribute('title','Remove the folder lock first');
  await expect(page.getByRole('dialog', {name:/^Move /})).toHaveCount(0);
  await page.keyboard.press('Escape');
  await page.getByRole('button', {name:'Expand Session private',exact:true}).click();
  await expect(page.getByRole('treeitem', {name:/Standup/})).not.toHaveAttribute('draggable','true');
});
