import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * The workspace hierarchy in the sidebar: Projects › Folders › collapsible
 * per-type groups.
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
        // Ten in the container, two on the first page — so the group header must
        // show 10 and a "see all" line must appear.
        total: 10,
        items: [
          { kind: "meeting", id: "m-standup", title: "Standup", durationS: 900, sortAt: 2 },
          { kind: "meeting", id: "m-retro", title: null, durationS: 1800, sortAt: 1 },
        ],
      },
    ],
  },
  {
    id: "p-private",
    name: "Prywatne",
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

async function openWorkspace(page: Page): Promise<void> {
  await mockTauri(page, {}, { list_workspace_tree: FOREST });
  await page.goto("/");
  const section = page.getByRole("button", { name: /Projekty/i }).first();
  await expect(section).toBeVisible();
}

test("renders projects, their folders and collapsible type groups", async ({ page }) => {
  await openWorkspace(page);

  const tree = page.getByRole("tree", { name: "Hierarchia obszaru roboczego" });
  await expect(tree).toBeVisible();

  await expect(page.getByRole("treeitem", { name: /Acme/ })).toBeVisible();
  await expect(page.getByRole("treeitem", { name: /Prywatne/ })).toBeVisible();

  // A collapsed project shows no groups.
  await expect(page.getByRole("treeitem", { name: /Spotkania/ })).toHaveCount(0);

  await page.getByRole("button", { name: "Expand Acme" }).click();

  // The group header carries the container's FULL count, not the page size.
  const meetings = page.getByRole("treeitem", { name: /Spotkania/ });
  await expect(meetings).toBeVisible();
  await expect(meetings).toContainText("10");

  // Items appear only once their group is expanded.
  await expect(page.getByRole("treeitem", { name: /Standup/ })).toHaveCount(0);
  await page.getByRole("button", { name: "Expand Spotkania" }).click();
  await expect(page.getByRole("treeitem", { name: /Standup/ })).toBeVisible();

  // An untitled item renders a placeholder rather than an empty row.
  await expect(page.getByRole("treeitem", { name: /Bez tytułu/ })).toBeVisible();

  // Ten total, two shown → the pager appears and names the remainder.
  await expect(page.getByRole("treeitem", { name: /Zobacz wszystkie \(10\)/ })).toBeVisible();

  // A child folder is rendered under its project, with its own groups.
  await expect(page.getByRole("treeitem", { name: /Q3/ })).toBeVisible();
});

test("a sealed project discloses nothing about what it holds", async ({ page }) => {
  await openWorkspace(page);

  const sealed = page.getByRole("treeitem", { name: /Prywatne/ });
  await expect(sealed).toBeVisible();

  // No counts: the backend refused to describe the contents, so the tree must
  // not imply it knows them — and "0" would be a claim, not an absence.
  await expect(sealed).not.toContainText(/\d/);

  // And no disclosure control, because there is nothing to disclose. Offering
  // one that expands to emptiness would read as "this project is empty".
  await expect(page.getByRole("button", { name: "Expand Prywatne" })).toHaveCount(0);
});
