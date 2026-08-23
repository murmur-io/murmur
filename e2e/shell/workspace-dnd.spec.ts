import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const FOREST = [
  {
    id: "p-acme",
    name: "Acme",
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
      {
        kind: "task",
        total: 1,
        items: [
          { kind: "task", id: "t-1", title: "Zadanie", durationS: null, sortAt: 1 },
        ],
      },
    ],
  },
  {
    id: "p-target",
    name: "Target",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
  {
    id: "p-sealed",
    name: "Clients",
    level: "project",
    emoji: null,
    tint: null,
    locked: true,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
];

async function open(page: Page): Promise<void> {
  // Tall enough that the whole tree fits: a rail that scrolls mid-drag moves the source
  // out from under the pointer, and the drag never completes.
  await page.setViewportSize({ width: 1280, height: 1400 });
  await mockTauri(
    page,
    {
      move_note: (args: unknown) => {
        (globalThis as unknown as { __moves: unknown[] }).__moves ??= [];
        (globalThis as unknown as { __moves: unknown[] }).__moves.push(args);
        return null;
      },
    },
    { list_workspace_tree: FOREST },
  );
  await page.goto("/");
  await expect(page.getByRole("tree", { name: "Workspace" })).toBeVisible();
  await page.getByRole("button", { name: "Expand Acme" }).click();
  await page.getByRole("button", { name: "Expand Meetings" }).click();
}

test("a meeting is draggable and a task is not", async ({ page }) => {
  await open(page);

  const meeting = page.getByRole("treeitem", { name: /Standup/ });
  await expect(meeting).toHaveAttribute("draggable", "true");

  // Neither a task nor a dashboard has a container anchor yet, so a drop would have
  // nowhere to file it — and a row that cannot be dropped anywhere must not look
  // draggable.
  await page.getByRole("button", { name: "Expand Tasks" }).click();
  await expect(page.getByRole("treeitem", { name: /Zadanie/ })).not.toHaveAttribute(
    "draggable",
    "true",
  );
});

test("dropping a meeting on a container files it there", async ({ page }) => {
  await open(page);

  // Grab the row by its BODY, the way a user does. The treeitem's centre can fall on the
  // trailing control, which is not the drag handle — and a drag that never starts looks
  // exactly like a drop that was refused.
  await page
    .getByRole("treeitem", { name: /Standup/ })
    .dragTo(page.getByRole("treeitem", { name: /Target/ }));

  const moves = await page.evaluate(
    () => (globalThis as unknown as { __moves?: unknown[] }).__moves ?? [],
  );
  expect(moves).toEqual([{ meetingId: "m-1", folderId: "p-target" }]);
});

test("a sealed container is not a drop target", async ({ page }) => {
  await open(page);

  await page
    .getByRole("treeitem", { name: /Standup/ })
    .dragTo(page.getByRole("treeitem", { name: /Clients/ }));

  // Every mover refuses a sealed, not-unlocked destination, so arming it would only
  // invite a drop that can fail.
  const moves = await page.evaluate(
    () => (globalThis as unknown as { __moves?: unknown[] }).__moves ?? [],
  );
  expect(moves).toEqual([]);
});
