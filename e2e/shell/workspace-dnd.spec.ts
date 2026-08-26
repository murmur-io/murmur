import { expect, test, type Page } from "@playwright/test";

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
      {
        kind: "task",
        total: 1,
        items: [
          { kind: "task", id: "t-1", title: "Ship the thing", durationS: null, sortAt: 1 },
        ],
      },
      {
        kind: "dashboard",
        total: 1,
        items: [
          { kind: "dashboard", id: "d-1", title: "Q3 board", durationS: null, sortAt: 0 },
        ],
      },
    ],
  },
  {
    id: "p-target",
    name: "Target",
    kind: "meeting",
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
    kind: "meeting",
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
        const w = globalThis as unknown as { __moves?: unknown[] };
        (w.__moves ??= []).push({ cmd: "move_note", args });
        return null;
      },
      move_note_doc: (args: unknown) => {
        const w = globalThis as unknown as { __moves?: unknown[] };
        (w.__moves ??= []).push({ cmd: "move_note_doc", args });
        return null;
      },
      move_dashboard_to_container: (args: unknown) => {
        const w = globalThis as unknown as { __moves?: unknown[] };
        (w.__moves ??= []).push({ cmd: "move_dashboard_to_container", args });
        return null;
      },
      set_task_container: (args: unknown) => {
        const w = globalThis as unknown as { __moves?: unknown[] };
        (w.__moves ??= []).push({ cmd: "set_task_container", args });
        return null;
      },
    },
    { list_workspace_tree: FOREST },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Spaces" }).click();
  await expect(page.getByRole("tree", { name: "Spaces" })).toBeVisible();
  await page.getByRole("button", { name: "Expand Acme" }).click();
}

test("every kind the tree renders is draggable", async ({ page }) => {
  await open(page);

  // This test used to assert the OPPOSITE for tasks and dashboards, and was right to: neither
  // had a container anchor, so a drag would have been a gesture with nothing behind it. Both
  // gained a backend mover, and a row a user can see under a project is a row they will try to
  // drag out of it.
  await expect(page.getByRole("treeitem", { name: /Standup/ })).toHaveAttribute(
    "draggable",
    "true",
  );

  await expect(page.getByRole("treeitem", { name: /Ship the thing/ })).toHaveAttribute(
    "draggable",
    "true",
  );

  await expect(page.getByRole("treeitem", { name: /Q3 board/ })).toHaveAttribute(
    "draggable",
    "true",
  );
});

test("each kind is filed through its OWN backend mover", async ({ page }) => {
  await open(page);

  // The four kinds do NOT share a command, and the id alone cannot say which one to call — a
  // single mover for all of them would file a board through the note path and lose it. Each
  // drag is checked for the command AND its argument shape, against the real invoke path.
  await page
    .getByRole("treeitem", { name: /Standup/ })
    .dragTo(page.getByRole("treeitem", { name: /Target/ }));

  await page
    .getByRole("treeitem", { name: /Q3 board/ })
    .dragTo(page.getByRole("treeitem", { name: /Target/ }));

  await page
    .getByRole("treeitem", { name: /Ship the thing/ })
    .dragTo(page.getByRole("treeitem", { name: /Target/ }));

  const moves = await page.evaluate(
    () => (globalThis as unknown as { __moves?: unknown[] }).__moves ?? [],
  );
  expect(moves).toEqual([
    { cmd: "move_note", args: { meetingId: "m-1", folderId: "p-target" } },
    {
      cmd: "move_dashboard_to_container",
      args: { id: "d-1", folderId: "p-target" },
    },
    { cmd: "set_task_container", args: { id: "t-1", containerId: "p-target" } },
  ]);
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
  expect(moves).toEqual([
    { cmd: "move_note", args: { meetingId: "m-1", folderId: "p-target" } },
  ]);
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
