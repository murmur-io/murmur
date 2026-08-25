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

const TASK = {
  id: "t-ship",
  orgId: "org-acme",
  docId: "doc-ship",
  itemId: "item-ship",
  sourceDocumentId: null,
  version: 1,
  title: "Ship release",
  description: "Keep the task route usable on a narrow window.",
  status: "todo",
  dueAt: null,
  assigneeUserId: null,
  createdAt: "2026-08-24T09:00:00Z",
  subtasks: [],
  orgRefs: [],
  images: [],
  access: "edit",
  canEdit: true,
  canManage: true,
  localRefs: [],
  updatedAt: "2026-08-24T09:00:00Z",
};

const ORGS = [
  {
    orgId: "org-acme",
    name: "Acme",
    role: "owner",
    memberCount: 1,
    consented: true,
    lastSeq: 1,
    itemCount: 1,
    receivedCount: 0,
    pendingShares: 0,
    contextEnabled: true,
  },
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
  await expect(page).toHaveURL(/\/record$/);
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
  await expect(page).toHaveURL(/\/library$/);
  await expect(page.getByRole("complementary", { name: "Spaces sidebar" })).toBeVisible();
});

test("opens Spaces without unmounting desktop Ask content", async ({ page }) => {
  await boot(page, "/ask");
  const askHeading = page.getByRole("heading", { name: "Ask your meetings" });
  await expect(askHeading).toBeVisible();

  await page.getByRole("button", { name: "Spaces" }).click();

  await expect(page).toHaveURL(/\/ask$/);
  await expect(askHeading).toBeVisible();
  await expect(page.getByRole("complementary", { name: "Spaces sidebar" })).toBeVisible();
});

for (const [count, label] of [
  [1, "Re-seal all 1 unlocked folder now"],
  [2, "Re-seal all 2 unlocked folders now"],
] as const) {
  test(`names the re-seal action correctly for ${count} unlocked folder${count === 1 ? "" : "s"}`, async ({
    page,
  }) => {
    const folders = Array.from({ length: count }, (_, index) => ({
      id: `f-${index}`,
      name: `Private ${index + 1}`,
      path: `Private ${index + 1}`,
      parentId: null,
      noteCount: 1,
      locked: true,
      unlocked: true,
      kind: "note",
      children: [],
    }));
    await mockTauri(
      page,
      {},
      { list_workspace_tree: FOREST, list_folders: folders },
    );
    await page.goto("/tasks/t-ship");

    await expect(page.getByRole("button", { name: label })).toBeVisible();
  });
}

test("keeps task navigation usable at 390px without a side-by-side context panel", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockTauri(
    page,
    {},
    {
      list_workspace_tree: FOREST,
      list_tasks: [TASK],
      org_list_statuses: ORGS,
    },
  );
  await page.goto("/tasks");

  await expect(page.locator(".app-sidebar")).toBeHidden();
  const mainBox = await page.locator(".main-col").boundingBox();
  expect(mainBox).not.toBeNull();
  expect(mainBox!.width).toBeGreaterThan(250);

  await page.locator('[data-task-id="t-ship"]').click();
  await expect(page).toHaveURL(/\/tasks\/t-ship$/);
  await expect(page.getByLabel("Task title")).toHaveValue("Ship release");
  await expect(page.locator(".app-sidebar")).toBeHidden();

  await page.getByRole("button", { name: "Spaces" }).click();
  await expect(page).toHaveURL(/\/container\/p-acme$/);
  await expect(page.locator(".app-sidebar")).toBeHidden();
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
