import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * The reserved note root renders as an INBOX, not a folder.
 *
 * `is_root` marks the one always-open note folder that backs the "Notes"
 * section — unfiled notes land there and it can never be sealed. The migration
 * that introduced it says the frontend hides it from the folder tree, because
 * it IS the section rather than a nested child. The 2026-08-22 hierarchy
 * rebuild renders every container `list_containers` returns, and that
 * predicate filters on kind and path but NOT on `is_root`, so the root came
 * back as a folder-shaped row on which rename, delete, lock and share are all
 * disabled — a row that looks broken because every action is missing.
 *
 * These are the oracles for the fix. Without them nothing notices if the root
 * reappears as a folder: it renders, it is clickable, and every existing spec
 * stays green.
 */
const NOTE_ROOT = {
  id: "f-notes-root",
  name: "Notes",
  kind: "note",
  level: "folder",
  emoji: null,
  tint: null,
  locked: false,
  unlocked: false,
  isRoot: true,
  folders: [
    {
      id: "f-child-of-root",
      name: "Kept child",
      kind: "note",
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
  groups: [
    {
      kind: "note",
      total: 12,
      items: [
        {
          kind: "note",
          id: "n-loose",
          title: "Loose thought",
          durationS: null,
          sortAt: 90,
        },
      ],
    },
  ],
};

const FOREST = [
  {
    id: "p-acme",
    name: "Acme",
    kind: "note",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [NOTE_ROOT],
    groups: [],
  },
];

async function openSidebar(page: Page): Promise<void> {
  await mockTauri(page, {}, { list_workspace_tree: FOREST });
  await page.goto("/");
  await expect(
    page.getByRole("navigation", { name: "Primary navigation" }),
  ).toBeVisible();
  await page
    .getByRole("treeitem", { name: /Acme/ })
    .getByRole("button", { name: /Expand/ })
    .click();
}

test("the reserved note root is not drawn as a folder row", async ({
  page,
}) => {
  await openSidebar(page);
  // The row itself must be absent, not merely action-free. Asserting only on the
  // ⋯ menu passed even with the skip disabled, which made this test a witness to
  // nothing — the menu is hover-revealed, so its absence proves neither state.
  // Case-sensitive on purpose: the root row would be named "Notes", while the
  // inbox row that replaces it reads "Unfiled notes". Anchoring with ^ does not
  // work here — a tree row's accessible name also folds in its caret and menu
  // labels ("Expand Notes …"), so the match has to be a substring.
  await expect(page.getByRole("treeitem", { name: /Notes/ })).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Actions for Notes" }),
  ).toHaveCount(0);
});

test("its notes render as an Unfiled notes inbox at the top level", async ({
  page,
}) => {
  await openSidebar(page);
  const inbox = page.getByRole("treeitem", { name: /Unfiled notes/ });
  await expect(inbox).toBeVisible();
  await expect(inbox).toHaveAttribute("aria-level", "1");
  await expect(inbox).toContainText("12");
  await expect(page.getByRole("treeitem", { name: /Loose thought/ })).toBeVisible();
  // The continuation row carries an explicit role="treeitem" (matching the
  // unfiled-recordings row it mirrors), so it is not queried as a button.
  await expect(
    page.getByRole("treeitem", { name: /View all \(12\)/ }),
  ).toBeVisible();
});

test("the inbox offers no folder affordance at all", async ({ page }) => {
  await openSidebar(page);
  const inbox = page.getByRole("treeitem", { name: /Unfiled notes/ });
  // An honest inbox, exactly like unfiled recordings: no lock, no manage, no
  // create-here, and nothing to share.
  await expect(
    inbox.getByRole("button", { name: /Actions for/ }),
  ).toHaveCount(0);
});

test("a folder the user created under the root stays reachable", async ({
  page,
}) => {
  // The root's own row is gone, but a container the user made must never become
  // unreachable because its parent stopped being drawn.
  await openSidebar(page);
  await expect(
    page.getByRole("treeitem", { name: /Kept child/ }),
  ).toBeVisible();
});
