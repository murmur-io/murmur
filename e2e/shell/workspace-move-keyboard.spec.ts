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
  await page.goto("/");
  await expect(page.getByRole("tree", { name: "Workspaces" })).toBeVisible();
  await page.getByRole("button", { name: "Expand Acme" }).click();
}

/**
 * Dragging is a pointer gesture with no keyboard form of its own, so a tree that
 * can only be reorganised by dragging cannot be reorganised without a mouse at all.
 *
 * Driven by FOCUS AND KEYS, not by clicking. A test named "without a pointer" that
 * reaches its control with a mouse is not testing what it says — and it inherits
 * every way a pointer click can be intercepted by whatever happens to be on top,
 * which is exactly how this spec failed on one CI lane while passing everywhere else.
 */
test("an item can be filed without a pointer", async ({ page }) => {
  await open(page);

  await page.getByRole("button", { name: "Actions for meeting Standup" }).focus();
  await page.keyboard.press("Enter");
  await page.getByRole("menuitem", { name: "Move to Workspace or folder…" }).focus();
  await page.keyboard.press("Enter");
  const sheet = page.getByRole("dialog", { name: "Move recording “Standup” to Workspace" });
  await expect(sheet).toBeVisible();
  await sheet.getByRole("button", { name: /Beta \/ Shared/ }).focus();
  await page.keyboard.press("Enter");

  const moves = await page.evaluate(
    () => (globalThis as unknown as { __moves?: unknown[] }).__moves ?? [],
  );
  expect(moves).toEqual([{ meetingId: "m-1", folderId: "f-beta-shared" }]);
});

test("the move sheet disambiguates duplicate names, filters full paths, and reports a failed move", async ({ page }) => {
  await open(page);

  await page.getByRole("button", { name: "Actions for meeting Standup" }).click();
  await page.getByRole("menuitem", { name: "Move to Workspace or folder…" }).click();
  const sheet = page.getByRole("dialog", { name: "Move recording “Standup” to Workspace" });
  await expect(sheet.getByRole("button", { name: /Beta \/ Shared/ })).toBeVisible();
  await expect(sheet.getByRole("button", { name: /Gamma \/ Shared/ })).toBeVisible();
  await expect(
    sheet.getByRole("button", { name: "Move to Beta / Archive (1)" }),
  ).toBeVisible();
  await expect(
    sheet.getByRole("button", { name: "Move to Beta / Archive (2)" }),
  ).toBeVisible();

  await sheet.getByRole("searchbox", { name: "Search destinations" }).fill("Gamma / Shared");
  await expect(sheet.getByRole("button", { name: /Gamma \/ Shared/ })).toBeVisible();
  await expect(sheet.getByRole("button", { name: /Beta \/ Shared/ })).toHaveCount(0);
  await sheet.getByRole("searchbox", { name: "Search destinations" }).fill("");

  // Every mover refuses a sealed, not-unlocked destination, so offering it would only
  // produce an error the user cannot act on.
  await expect(sheet.getByRole("button", { name: /^Clients/ })).toHaveCount(0);
  // And moving something to where it already is is not a move.
  await expect(sheet.getByRole("button", { name: /^Acme/ })).toHaveCount(0);

  const failedTarget = sheet.getByRole("button", {
    name: "Move to Beta / Archive (1)",
  });
  await expect
    .poll(() =>
      failedTarget.evaluate((element) => {
        const box = element.getBoundingClientRect();
        const hit = document.elementFromPoint(
          box.left + box.width / 2,
          box.top + box.height / 2,
        );
        return hit === element || Boolean(hit && element.contains(hit));
      }),
    )
    .toBe(true);
  await failedTarget.click();
  await expect(sheet.getByRole("alert")).toContainText(
    "Couldn’t move “Standup” to Beta / Archive (1)",
  );
  await expect(sheet).toBeVisible();
});

test("the move picker never exposes stale descendants below a sealed Workspace", async ({ page }) => {
  await open(page);

  await page.getByRole("button", { name: "Actions for meeting Standup" }).click();
  await page.getByRole("menuitem", { name: "Move to Workspace or folder…" }).click();
  const sheet = page.getByRole("dialog", { name: "Move recording “Standup” to Workspace" });
  await expect(sheet).toBeVisible();
  await expect(sheet).not.toContainText("Secret child");
  await expect(sheet.getByRole("button", { name: "Move to Clients / Secret child" })).toHaveCount(0);
});
