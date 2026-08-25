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

const PLAN = {
  moves: [
    {
      noteId: "n-1",
      title: "Standup 14 Aug",
      fromFolder: "Acme",
      toFolder: "Standups",
      toFolderId: null,
    },
  ],
};

async function open(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1280, height: 1000 });
  await mockTauri(
    page,
    {
      plan_organize_notes: (args: unknown) => {
        const w = globalThis as unknown as { __planned?: unknown[]; __plan: unknown };
        (w.__planned ??= []).push(args);
        return w.__plan;
      },
      apply_organize_plan: (args: unknown) => {
        const w = globalThis as unknown as { __applied?: unknown[] };
        (w.__applied ??= []).push(args);
        return null;
      },
    },
    { list_workspace_tree: FOREST },
  );
  await page.addInitScript((plan) => {
    (globalThis as unknown as { __plan: unknown }).__plan = plan;
  }, PLAN);
  await page.goto("/");
  await page.getByRole("button", { name: "Spaces" }).click();
  await expect(page.getByRole("tree", { name: "Spaces" })).toBeVisible();
}

/**
 * The AI organizer is scoped to the CONTAINER the user asked about.
 *
 * The planner and its review sheet already existed, reachable only from the Notes home header and
 * scoped to whichever note-folder happened to be active. The thing a user wants to tidy is a
 * project or a folder, and the hierarchy is where those are named — so the action moved to the
 * container's own actions menu, and the container id has to travel with it. A planner called with
 * the wrong scope reads the wrong notes and proposes moves for files the user was not looking at.
 */
test("organizing a container plans for THAT container", async ({ page }) => {
  await open(page);

  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.locator("[data-testid='organize-container']").click();

  const planned = await page.evaluate(
    () => (globalThis as unknown as { __planned?: unknown[] }).__planned ?? [],
  );
  expect(planned).toEqual([{ folderId: "p-acme" }]);

  // Non-destructive: the review sheet is up and NOTHING has moved yet. An AI that silently
  // re-filed a vault would be a feature nobody could trust twice.
  await expect(page.getByText("Standups")).toBeVisible();
  const appliedBeforeConfirm = await page.evaluate(
    () => (globalThis as unknown as { __applied?: unknown[] }).__applied ?? [],
  );
  expect(appliedBeforeConfirm).toEqual([]);
});

/**
 * A SEALED container does not offer the action.
 *
 * The planner reads titles and body excerpts to classify them, and those reads are gated — so for
 * a sealed container it can only ever return an empty plan. Offering an action that cannot do
 * anything is worse than not offering it: the user reads the empty result as the feature failing.
 */
test("a sealed container does not offer the organizer", async ({ page }) => {
  await open(page);

  await page.getByRole("button", { name: "Actions for Clients" }).click();
  await expect(page.locator("[data-testid='organize-container']")).toHaveCount(0);
});
