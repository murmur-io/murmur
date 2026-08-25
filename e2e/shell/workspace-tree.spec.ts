import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * The workspace hierarchy in the contextual sidebar: Spaces › Folders ›
 * mixed content rows.
 *
 * The forest below is written as the BACKEND serializes it (`ContainerNode` /
 * `TypeGroup` / `ItemRow` carry `rename_all = "camelCase"`), not as the
 * component would find convenient. A hand-written mock defines a shape; it does
 * not verify one — so the only thing that makes this fixture meaningful is that
 * every key here was copied from `src-tauri/src/storage/models.rs`. The
 * serialized-key oracle on the Rust side is what proves the backend agrees.
 */
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
    folders: [
      {
        id: "f-q3",
        name: "Q3",
        level: "folder",
        emoji: null,
        tint: null,
        locked: false,
        unlocked: false,
        isRoot: false,
        folders: [],
        groups: [
          {
            kind: "note",
            total: 1,
            items: [
              { kind: "note", id: "n-plan", title: "Plan Q3", durationS: null, sortAt: 3 },
            ],
          },
        ],
      },
    ],
    groups: [
      {
        kind: "meeting",
        total: 5,
        items: [
          { kind: "meeting", id: "m-standup", title: "Standup", durationS: 900, sortAt: 90 },
          { kind: "meeting", id: "m-retro", title: null, durationS: 1800, sortAt: 40 },
          { kind: "meeting", id: "m-old", title: "Old sync", durationS: 600, sortAt: 10 },
        ],
      },
      {
        kind: "note",
        total: 3,
        items: [
          { kind: "note", id: "n-brief", title: "Launch brief", durationS: null, sortAt: 100 },
          { kind: "note", id: "n-risks", title: "Risks", durationS: null, sortAt: 60 },
        ],
      },
      {
        kind: "task",
        total: 2,
        items: [
          { kind: "task", id: "t-ship", title: "Ship release", durationS: null, sortAt: 80 },
          { kind: "task", id: "t-copy", title: "Review copy", durationS: null, sortAt: 30 },
        ],
      },
      {
        kind: "dashboard",
        total: 2,
        items: [
          { kind: "dashboard", id: "d-release", title: "Release dashboard", durationS: null, sortAt: 70 },
          { kind: "dashboard", id: "d-metrics", title: "Metrics", durationS: null, sortAt: 50 },
        ],
      },
    ],
  },
  {
    id: "p-private",
    name: "Private",
    level: "project",
    emoji: null,
    tint: null,
    // Sealed and NOT session-unlocked: the backend sends no groups at all, not
    // even totals.
    locked: true,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
];

async function expectMenuItemAtHitPoint(page: Page, itemName: string): Promise<void> {
  const item = page.getByRole("menuitem", { name: itemName });
  await expect(item).toBeVisible();
  await expect
    .poll(() =>
      item.evaluate((element) => {
        const box = element.getBoundingClientRect();
        const hit = document.elementFromPoint(
          box.left + box.width / 2,
          box.top + box.height / 2,
        );
        return hit === element || Boolean(hit && element.contains(hit));
      }),
    )
    .toBe(true);
}

async function openWorkspace(page: Page): Promise<void> {
  await mockTauri(page, {}, { list_workspace_tree: FOREST });
  await page.goto("/");
  await page.getByRole("button", { name: "Spaces" }).click();
  await expect(page.getByRole("complementary", { name: "Spaces sidebar" })).toBeVisible();
}

test("keeps both container menus above the following tree rows", async ({ page }) => {
  await openWorkspace(page);
  await page.getByRole("button", { name: "Expand Acme" }).click();

  await page.getByRole("button", { name: "Add to Acme" }).click();
  await expectMenuItemAtHitPoint(page, "New note");
  await page.keyboard.press("Escape");

  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await expectMenuItemAtHitPoint(page, "Rename");
});

test("shows unfiled recordings as a real inbox and opens the complete meetings list", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      list_container_items: (args: {
        containerId: string | null;
        kind: string;
        offset: number;
        limit: number;
      }) => {
        if (
          args.containerId !== null ||
          args.kind !== "meeting" ||
          args.offset !== 0 ||
          args.limit !== 8
        ) {
          throw new Error("unexpected unfiled page request");
        }
        return {
          kind: "meeting",
          total: 12,
          items: Array.from({ length: 8 }, (_, index) => ({
            kind: "meeting",
            id: `m-unfiled-${12 - index}`,
            title: `Unfiled recording ${12 - index}`,
            durationS: 600 + index,
            sortAt: 120 - index,
          })),
        };
      },
    },
    { list_workspace_tree: FOREST },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Spaces" }).click();

  const inbox = page.getByRole("treeitem", { name: /Unfiled recordings/ });
  await expect(inbox).toBeVisible();
  await expect(inbox).toContainText("12");
  await expect(inbox.getByRole("button", { name: /Add to/ })).toHaveCount(0);
  await expect(inbox.getByRole("button", { name: /Actions for/ })).toHaveCount(0);
  await expect(inbox).not.toHaveAttribute("appfolderdrop");

  const tree = page.getByRole("tree", { name: "Spaces" });
  const unfiledRows = tree.locator(".line--unfiled-item");
  await expect(unfiledRows).toHaveCount(8);
  await expect(unfiledRows).toHaveText([
    "Unfiled recording 12",
    "Unfiled recording 11",
    "Unfiled recording 10",
    "Unfiled recording 9",
    "Unfiled recording 8",
    "Unfiled recording 7",
    "Unfiled recording 6",
    "Unfiled recording 5",
  ]);
  const moveNewest = page.getByRole("button", {
    name: "Move Unfiled recording 12",
  });
  await expect(moveNewest).toBeVisible();
  await moveNewest.click();
  await expect(page.getByRole("menuitem", { name: "Acme", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");

  await page.getByRole("button", { name: "Collapse Unfiled recordings" }).click();
  await expect(unfiledRows).toHaveCount(0);
  await page.getByRole("button", { name: "Expand Unfiled recordings" }).click();
  await expect(unfiledRows).toHaveCount(8);

  // Prove the destination clears an existing meeting-folder scope instead of
  // merely changing the URL while Library remains filtered to that Space.
  await page
    .getByRole("treeitem", { name: /Acme/ })
    .getByRole("button", { name: "Acme", exact: true })
    .click();
  await expect(page).toHaveURL(/\/container\/p-acme$/);

  await page.getByRole("treeitem", { name: "View all recordings (12)" }).click();
  await expect(page).toHaveURL(/\/library$/);
  await expect(page.getByRole("heading", { name: "Meetings", exact: true })).toBeVisible();
});

test("scrubs unfiled titles synchronously and drops a late pre-invalidation page", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      list_container_items: () => {
        const target = window as unknown as {
          __unfiledCalls?: number;
          __releaseLateUnfiled?: () => void;
        };
        target.__unfiledCalls = (target.__unfiledCalls ?? 0) + 1;
        if (target.__unfiledCalls === 1) {
          return {
            kind: "meeting",
            total: 1,
            items: [
              {
                kind: "meeting",
                id: "m-mounted-secret",
                title: "Mounted private recording",
                durationS: 300,
                sortAt: 10,
              },
            ],
          };
        }
        if (target.__unfiledCalls === 2) {
          return new Promise((resolve) => {
            target.__releaseLateUnfiled = () =>
              resolve({
                kind: "meeting",
                total: 1,
                items: [
                  {
                    kind: "meeting",
                    id: "m-late-secret",
                    title: "Late private recording",
                    durationS: 300,
                    sortAt: 20,
                  },
                ],
              });
          });
        }
        // The repair read must not hide a stale-response bug by winning the
        // race and replacing call #2 with a safe empty page. A real privacy
        // transition may be followed by an unavailable reader, so the
        // generation guard itself has to keep the late secret from landing.
        return Promise.reject(new Error("post-invalidation refresh unavailable"));
      },
    },
    { list_workspace_tree: FOREST },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Spaces" }).click();
  await expect(page.getByText("Mounted private recording", { exact: true })).toBeVisible();

  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://reminder-visibility-invalidated", null);
  });
  await expect(page.getByText("Mounted private recording", { exact: true })).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __unfiledCalls?: number }).__unfiledCalls ?? 0,
      ),
    )
    .toBe(2);

  await page.evaluate(() => {
    const target = window as unknown as { __releaseLateUnfiled?: () => void };
    target.__releaseLateUnfiled?.();
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://reminder-visibility-invalidated", null);
  });
  await expect(page.getByText("Late private recording", { exact: true })).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __unfiledCalls?: number }).__unfiledCalls ?? 0,
      ),
    )
    .toBe(3);
});

test("renders one flat mixed stream below each expanded container", async ({ page }) => {
  await openWorkspace(page);

  const tree = page.getByRole("tree", { name: "Spaces" });
  await expect(tree).toBeVisible();

  await expect(page.getByRole("treeitem", { name: /Acme/ })).toBeVisible();
  await expect(page.getByRole("treeitem", { name: /Private/ })).toBeVisible();

  // A selected Space can still be collapsed by the user.
  await expect(page.getByRole("treeitem", { name: /Launch brief/ })).toHaveCount(0);
  await page.getByRole("button", { name: "Expand Acme" }).click();

  // No synthetic kind headers: direct children are globally newest-first.
  await expect(tree.locator(".line--group")).toHaveCount(0);
  const mixedRows = tree.locator(".line--item");
  await expect(mixedRows).toHaveCount(8);
  await expect(mixedRows).toHaveText([
    "Launch brief",
    "Standup",
    "Ship release",
    "Release dashboard",
    "Risks",
    "Metrics",
    "Untitled",
    "Review copy",
  ]);
  for (const row of await mixedRows.all()) {
    await expect(row).toHaveAttribute("aria-level", "2");
  }

  // An untitled item renders a placeholder rather than an empty row.
  await expect(page.getByRole("treeitem", { name: /Untitled/ })).toBeVisible();

  // All kinds share one total and one continuation row.
  await expect(page.getByRole("treeitem", { name: /View all \(12\)/ })).toHaveCount(1);

  // A child folder is rendered under its project, with its own groups.
  await expect(page.getByRole("treeitem", { name: /Q3/ })).toBeVisible();
});

test("keeps an older selected leaf within the eight-row cap", async ({ page }) => {
  await mockTauri(page, {}, { list_workspace_tree: FOREST });
  await page.goto("/meeting/m-old");

  const tree = page.getByRole("tree", { name: "Spaces" });
  await expect(tree).toBeVisible();
  const mixedRows = tree.locator(".line--item");
  await expect(mixedRows).toHaveCount(8);
  await expect(mixedRows).toHaveText([
    "Launch brief",
    "Standup",
    "Ship release",
    "Release dashboard",
    "Risks",
    "Metrics",
    "Untitled",
    "Old sync",
  ]);
  await expect(page.getByRole("treeitem", { name: "Old sync" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.getByRole("treeitem", { name: /View all \(12\)/ })).toHaveCount(1);
});

test("a sealed project discloses nothing about what it holds", async ({ page }) => {
  await openWorkspace(page);

  const sealed = page.getByRole("treeitem", { name: /Private/ });
  await expect(sealed).toBeVisible();

  // No counts: the backend refused to describe the contents, so the tree must
  // not imply it knows them — and "0" would be a claim, not an absence.
  await expect(sealed).not.toContainText(/\d/);

  // And no disclosure control, because there is nothing to disclose. Offering
  // one that expands to emptiness would read as "this project is empty".
  await expect(page.getByRole("button", { name: "Expand Private" })).toHaveCount(0);
});

test("treats a sealed container as an intrinsic leaf even when a stale payload includes content", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      list_workspace_tree: [
        {
          id: "p-sealed-stale",
          name: "Private",
          level: "project",
          emoji: "🔒",
          tint: "violet",
          locked: true,
          unlocked: false,
          isRoot: false,
          folders: [
            {
              id: "f-secret",
              name: "Secret child",
              level: "folder",
              emoji: null,
              tint: null,
              locked: true,
              unlocked: false,
              isRoot: false,
              folders: [],
              groups: [],
            },
          ],
          groups: [
            {
              kind: "note",
              total: 1,
              items: [
                {
                  kind: "note",
                  id: "n-secret",
                  title: "Secret launch title",
                  durationS: null,
                  sortAt: 1,
                },
              ],
            },
          ],
        },
      ],
    },
  );
  await page.goto("/container/p-sealed-stale");

  const sealed = page.getByRole("treeitem", { name: "Private" });
  await expect(sealed).toBeVisible();
  await expect(page.getByRole("button", { name: "Expand Private" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Add to Private" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Actions for Private" })).toHaveCount(1);
  await page.getByRole("button", { name: "Actions for Private" }).click();
  await expect(page.getByRole("menuitem", { name: "Unlock for this session" })).toBeVisible();
  await expect(page.getByRole("menuitem")).toHaveCount(1);
  await expect(page.getByText("Secret child", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Secret launch title", { exact: true })).toHaveCount(0);
});

test("scrubs cached hierarchy titles when relock succeeds even if every refresh rejects", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      list_workspace_tree: () => {
        const target = window as unknown as { __workspaceTreeCalls?: number };
        target.__workspaceTreeCalls = (target.__workspaceTreeCalls ?? 0) + 1;
        if (target.__workspaceTreeCalls > 1) {
          return Promise.reject(new Error("workspace refresh unavailable"));
        }
        return [
          {
            id: "p-private",
            name: "Private",
            level: "project",
            emoji: null,
            tint: null,
            locked: true,
            unlocked: true,
            isRoot: false,
            folders: [],
            groups: [
              {
                kind: "note",
                total: 1,
                items: [
                  {
                    kind: "note",
                    id: "n-secret",
                    title: "Acquisition codename",
                    durationS: null,
                    sortAt: 1,
                  },
                ],
              },
              {
                kind: "meeting",
                total: 1,
                items: [
                  {
                    kind: "meeting",
                    id: "m-secret",
                    title: "Board compensation",
                    durationS: 600,
                    sortAt: 2,
                  },
                ],
              },
            ],
          },
        ];
      },
      relock_all: () => {
        (
          window as unknown as {
            __demoEmit: (event: string, payload: unknown) => void;
          }
        ).__demoEmit("murmur://reminder-visibility-invalidated", null);
        return null;
      },
    },
    {
      list_folders: [
        {
          id: "p-private",
          name: "Private",
          path: "Private",
          parentId: null,
          noteCount: 2,
          locked: true,
          unlocked: true,
          kind: "meeting",
          children: [],
        },
      ],
    },
  );
  await page.goto("/meeting/m-secret");

  await expect(page.getByText("Board compensation", { exact: true })).toBeVisible();
  await expect(page.getByText("Acquisition codename", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Re-seal all 1 unlocked folder now" }).click();

  await expect(page.getByText("Board compensation", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Acquisition codename", { exact: true })).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __workspaceTreeCalls?: number })
            .__workspaceTreeCalls ?? 0,
      ),
    )
    .toBeGreaterThan(1);
  await expect(page.getByText("Board compensation", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Acquisition codename", { exact: true })).toHaveCount(0);
});
