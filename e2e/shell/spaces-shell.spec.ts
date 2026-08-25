import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const FOREST = [
  {
    id: "p-acme",
    name: "Acme",
    level: "project",
    emoji: "🟣",
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [
      {
        kind: "task",
        total: 1,
        items: [
          { kind: "task", id: "t-ship", title: "Ship release", durationS: null, sortAt: 2 },
        ],
      },
      {
        kind: "dashboard",
        total: 1,
        items: [
          { kind: "dashboard", id: "d-release", title: "Release dashboard", durationS: null, sortAt: 1 },
        ],
      },
    ],
  },
];

const SEALED_FIRST_FOREST = [
  {
    ...FOREST[0],
    id: "p-sealed",
    name: "Private",
    locked: true,
    unlocked: false,
    groups: [],
  },
  FOREST[0],
];

async function boot(page: Page, path = "/record"): Promise<void> {
  await mockTauri(page, {}, { list_workspace_tree: FOREST });
  await page.goto(path);
}

test("uses a persistent global rail beside a contextual Spaces panel", async ({ page }) => {
  await boot(page);

  const rail = page.getByRole("navigation", { name: "Global navigation" });
  await expect(rail).toBeVisible();
  await expect(page.getByRole("complementary", { name: "Spaces sidebar" })).toHaveCount(0);
  await expect(rail.getByRole("link", { name: "Capture" })).toHaveAttribute(
    "aria-current",
    "page",
  );

  await rail.getByRole("button", { name: "Spaces" }).click();
  const spaces = page.getByRole("complementary", { name: "Spaces sidebar" });
  await expect(spaces).toBeVisible();
  await expect(spaces.getByRole("tree", { name: "Spaces" })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Primary" })).toHaveCount(0);
  await expect(page.locator(".pill-bar")).toHaveCount(0);

  const railBox = await rail.boundingBox();
  const spacesBox = await spaces.boundingBox();
  const mainBox = await page.locator(".main-col").boundingBox();
  expect(railBox).not.toBeNull();
  expect(spacesBox).not.toBeNull();
  expect(mainBox).not.toBeNull();
  expect(railBox!.width).toBeGreaterThanOrEqual(68);
  expect(railBox!.width).toBeLessThanOrEqual(74);
  expect(spacesBox!.width).toBeGreaterThanOrEqual(252);
  expect(spacesBox!.width).toBeLessThanOrEqual(258);
  expect(spacesBox!.x - (railBox!.x + railBox!.width)).toBe(8);
  expect(railBox!.x + railBox!.width).toBeLessThan(spacesBox!.x);
  expect(spacesBox!.x + spacesBox!.width).toBeLessThan(mainBox!.x);

  await expect(rail.getByRole("button", { name: "Search" })).toBeVisible();
  await expect(rail.getByRole("link", { name: "Capture" })).toBeVisible();
  await expect(rail.getByRole("button", { name: "Spaces" })).toBeVisible();
  await expect(rail.getByRole("link", { name: "Ask" })).toBeVisible();
  await expect(rail.getByRole("button", { name: "Browse" })).toBeVisible();
  await expect(rail.getByRole("link", { name: "Settings" })).toBeVisible();
});

test("task and dashboard leaves keep Spaces visible and select the matching row", async ({
  page,
}) => {
  await boot(page, "/tasks/t-ship");
  await expect(page).toHaveURL(/\/tasks\/t-ship$/);
  await expect(page.getByRole("complementary", { name: "Spaces sidebar" })).toBeVisible();
  await expect(page.getByRole("treeitem", { name: /Ship release/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );

  await page.getByRole("treeitem", { name: /Release dashboard/ }).click();
  await expect(page).toHaveURL(/\/dashboards\/d-release$/);
  await expect(page.getByRole("complementary", { name: "Spaces sidebar" })).toBeVisible();
  await expect(page.getByRole("treeitem", { name: /Release dashboard/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );
});

test("rail surfaces switch from deep content and Settings through real routes", async ({
  page,
}) => {
  await boot(page, "/settings");
  await page.getByRole("button", { name: "Spaces" }).click();
  await expect(page).toHaveURL(/\/container\/p-acme$/);
  await expect(page.getByRole("complementary", { name: "Spaces sidebar" })).toBeVisible();

  await page.getByRole("button", { name: "Browse" }).click();
  await expect(page).toHaveURL(/\/library$/);
  await expect(page.getByRole("complementary", { name: "Browse sidebar" })).toBeVisible();

  await page.getByRole("button", { name: "Spaces" }).click();
  await expect(page).toHaveURL(/\/container\/p-acme$/);
  await expect(page.getByRole("complementary", { name: "Spaces sidebar" })).toBeVisible();
});

test("does not offer or dispatch top-level folder creation into a sealed first Space", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      create_folder: (args: unknown) => {
        const target = window as unknown as { __shellFolderCalls?: unknown[] };
        (target.__shellFolderCalls ??= []).push(args);
        return null;
      },
    },
    { list_workspace_tree: SEALED_FIRST_FOREST },
  );
  await page.goto("/container/p-sealed");

  const create = page.getByRole("button", { name: "New folder in first Space" });
  await expect.soft(create).toHaveCount(0);
  if ((await create.count()) > 0) {
    await create.click();
  }
  const calls = await page.evaluate(
    () =>
      (window as unknown as { __shellFolderCalls?: unknown[] })
        .__shellFolderCalls ?? [],
  );
  expect(calls).toEqual([]);
});
