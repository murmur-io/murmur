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
    groups: [
      {
        kind: "meeting",
        total: 2,
        items: [
          { kind: "meeting", id: "m-1", title: "Standup", durationS: 900, sortAt: 2 },
        ],
      },
    ],
  },
];

const CONTAINER = {
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
};

async function open(page: Page): Promise<void> {
  await mockTauri(
    page,
    {
      list_container_items: (args: { kind: string }) =>
        args.kind === "meeting"
          ? {
              kind: "meeting",
              total: 2,
              items: [
                { kind: "meeting", id: "m-1", title: "Standup", durationS: 900, sortAt: 2 },
                { kind: "meeting", id: "m-2", title: null, durationS: 0, sortAt: 1 },
              ],
            }
          : { kind: args.kind, total: 0, items: [] },
    },
    { list_workspace_tree: FOREST, get_container: CONTAINER },
  );
  await page.goto("/");
}

/**
 * A wrong route does not fail — Angular's catch-all redirects to /record — so
 * "the click did nothing visible" and "the click opened the recorder" look the
 * same from outside. These assert the URL, which is the only thing that tells
 * them apart.
 */
test("a container row opens that container, not the recorder", async ({ page }) => {
  await open(page);

  // The row-main button, NOT the caret: the caret is also a button and it only expands.
  await page.getByRole("button", { name: "Acme", exact: true }).click();

  await expect(page).toHaveURL(/\/container\/p-acme$/);
  await expect(page.getByRole("heading", { name: /Acme/ })).toBeVisible();
  await expect(
    page.locator(".app-main").getByRole("button", { name: /Standup/ }),
  ).toBeVisible();
  // The kind with no items renders no section at all, rather than an empty one.
  await expect(page.getByRole("heading", { name: "Tasks" })).toHaveCount(0);
});

test("a note row opens the note route that actually exists", async ({ page }) => {
  await mockTauri(
    page,
    {},
    {
      list_workspace_tree: [
        {
          ...FOREST[0],
          groups: [
            {
              kind: "note",
              total: 1,
              items: [
                { kind: "note", id: "n-1", title: "Plan", durationS: null, sortAt: 1 },
              ],
            },
          ],
        },
      ],
    },
  );
  await page.goto("/");

  await page.getByRole("button", { name: "Expand Acme" }).click();
  await page.getByRole("button", { name: "Plan", exact: true }).click();

  // `/note/:id` does not exist; `/notes/:id` does. The wrong one silently lands
  // on /record.
  await expect(page).toHaveURL(/\/notes\/n-1$/);
});
