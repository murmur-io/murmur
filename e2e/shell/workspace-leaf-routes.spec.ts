import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Workspace LEAF routes: opening a task, a dashboard or Ask keeps the one
 * sidebar's Workspaces tree on screen with the matching row selected, and a
 * sealed first Workspace still refuses top-level folder creation.
 *
 * The two-panel assertions this file used to carry (a 68-74px icon rail beside a
 * 252-258px contextual panel, the panel's own footer collapse, the drilldown
 * redirect off /settings, and the 390px "no side-by-side panel" rule) described
 * a shell that no longer exists. Their live equivalents are in
 * `sidebar-shell.spec.ts`.
 */

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

test("task and dashboard leaves keep Workspaces visible and select the matching row", async ({
  page,
}) => {
  await boot(page, "/tasks/t-ship");
  await expect(page).toHaveURL(/\/tasks\/t-ship$/);
  await expect(page.getByRole("navigation", { name: "Primary navigation" })).toBeVisible();
  await expect(page.getByRole("treeitem", { name: /Ship release/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );

  await page.getByRole("treeitem", { name: /Release dashboard/ }).click();
  await expect(page).toHaveURL(/\/dashboards\/d-release$/);
  await expect(page.getByRole("navigation", { name: "Primary navigation" })).toBeVisible();
  await expect(page.getByRole("treeitem", { name: /Release dashboard/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );
});

test("the Workspaces tree renders beside Ask without unmounting it", async ({ page }) => {
  await boot(page, "/ask");
  const askHeading = page.getByRole("heading", { name: "Ask your meetings" });
  await expect(askHeading).toBeVisible();

  // The tree is permanent chrome now rather than a panel Ask had to make room
  // for: both are on screen at once, and Ask stays mounted.
  const sidebar = page.getByRole("navigation", { name: "Primary navigation" });
  await expect(sidebar.getByRole("tree", { name: "Workspaces" })).toBeVisible();
  await expect(page).toHaveURL(/\/ask$/);
  await expect(askHeading).toBeVisible();
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

test("does not offer or dispatch top-level folder creation into a sealed first Workspace", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      create_folder: (args: unknown) => {
        const target = window as unknown as { __shellFolderCalls?: unknown[] };
        (target.__shellFolderCalls ??= []).push(args);
        return { id: "f-new" };
      },
    },
    { list_workspace_tree: SEALED_FIRST_FOREST },
  );
  await page.goto("/container/p-sealed");

  const create = page.getByRole("button", { name: "New folder in first Workspace" });
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
