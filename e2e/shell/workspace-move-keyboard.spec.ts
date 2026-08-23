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
    ],
  },
  {
    id: "p-target",
    name: "Docelowy",
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
    name: "Klienci",
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
  await expect(page.getByRole("tree", { name: "Hierarchia obszaru roboczego" })).toBeVisible();
  await page.getByRole("button", { name: "Expand Acme" }).click();
  await page.getByRole("button", { name: "Expand Spotkania" }).click();
}

/**
 * Dragging is a pointer gesture with no keyboard form of its own, so a tree that
 * can only be reorganised by dragging cannot be reorganised without a mouse at all.
 */
test("an item can be filed without a pointer", async ({ page }) => {
  await open(page);

  await page.getByRole("button", { name: "Przenieś Standup" }).click();
  await page.getByRole("menuitem", { name: "Docelowy" }).click();

  const moves = await page.evaluate(
    () => (globalThis as unknown as { __moves?: unknown[] }).__moves ?? [],
  );
  expect(moves).toEqual([{ meetingId: "m-1", folderId: "p-target" }]);
});

test("the move menu offers neither a sealed container nor the current one", async ({ page }) => {
  await open(page);

  await page.getByRole("button", { name: "Przenieś Standup" }).click();

  // Every mover refuses a sealed, not-unlocked destination, so offering it would only
  // produce an error the user cannot act on.
  await expect(page.getByRole("menuitem", { name: "Klienci" })).toHaveCount(0);
  // And moving something to where it already is is not a move.
  await expect(page.getByRole("menuitem", { name: "Acme" })).toHaveCount(0);
  await expect(page.getByRole("menuitem", { name: "Docelowy" })).toBeVisible();
});
