import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const OPEN_PROJECT = {
  id: "p-acme",
  name: "Acme",
  kind: "note",
  level: "project",
  emoji: null,
  tint: null,
  locked: false,
  unlocked: false,
  isRoot: false,
  folders: [
    {
      id: "f-team",
      name: "Shared",
      kind: "note",
      level: "folder",
      emoji: null,
      tint: null,
      locked: false,
      unlocked: false,
      isRoot: false,
      folders: [
        {
          id: "f-team-deep",
          name: "Planning",
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
      groups: [],
    },
  ],
  groups: [],
};

const SEALED_PROJECT = {
  ...OPEN_PROJECT,
  id: "p-sealed",
  name: "Clients",
  locked: true,
  unlocked: false,
  folders: [
    {
      ...OPEN_PROJECT.folders[0],
      id: "f-stale-secret",
      name: "Secret child",
      folders: [],
    },
  ],
};

async function open(page: Page, forest: unknown[]): Promise<void> {
  await mockTauri(
    page,
    {
      create_note: (args: unknown) => {
        const target = window as unknown as { __noteCalls?: unknown[] };
        (target.__noteCalls ??= []).push(args);
        return "n-new";
      },
      create_note_folder: (args: unknown) => {
        const target = window as unknown as { __noteFolderCalls?: unknown[] };
        (target.__noteFolderCalls ??= []).push(args);
        const spec = args as { name: string; parentId: string | null };
        return {
          id: "nf-new",
          name: spec.name,
          path: `Notes/${spec.name}`,
          parentId: spec.parentId,
          locked: false,
          kind: "note",
        };
      },
      create_space: (args: unknown) => {
        const target = window as unknown as {
          __spaceCalls?: unknown[];
        };
        (target.__spaceCalls ??= []).push(args);
        const index = target.__spaceCalls.length;
        return {
          id: `p-new-${index}`,
          name: (args as { name: string }).name,
          path: `spaces/${index}`,
          parentId: null,
          locked: false,
          createdAt: "2026-08-26T00:00:00Z",
        };
      },
      create_folder: () => ({
        id: "f-new",
        name: "New folder",
        path: "Acme/New folder",
        parentId: "p-acme",
        locked: false,
        createdAt: "2026-08-23T00:00:00Z",
      }),
      create_dashboard: (args: unknown) => {
        const target = window as unknown as { __createDashboardCalls?: unknown[] };
        (target.__createDashboardCalls ??= []).push(args);
        return { id: "d-new" };
      },
    },
    { list_workspace_tree: forest },
  );
  await page.goto("/");
  await expect(page.getByRole("tree", { name: "Workspaces" })).toBeVisible();
}

test("header New opens an explicit sheet without writing, then creates the chosen type at a full path", async ({
  page,
}) => {
  await open(page, [OPEN_PROJECT]);

  await page.getByRole("button", { name: "Create in Workspaces" }).click();
  const sheet = page.getByRole("dialog", { name: "Create in Workspaces" });
  await expect(sheet).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __createDashboardCalls?: unknown[] })
          .__createDashboardCalls ?? [],
    ),
  ).toEqual([]);

  await sheet.getByRole("button", { name: "Dashboard", exact: true }).click();
  await sheet.getByLabel("Name").fill("Roadmap pulse");
  const destination = sheet.getByRole("button", {
    name: /Acme \/ Shared \/ Planning/,
  });
  await expect(destination).toBeVisible();
  await destination.click();
  await sheet.getByRole("button", { name: "Create dashboard" }).click();

  await expect(page).toHaveURL(/\/dashboards\/d-new$/);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __createDashboardCalls?: unknown[] })
            .__createDashboardCalls ?? [],
      ),
    )
    .toEqual([
      {
        title: "Roadmap pulse",
        emoji: null,
        tint: null,
        folderId: "f-team-deep",
      },
    ]);
});

test("a note can be created into a container and opens straight away", async ({ page }) => {
  await open(page, [OPEN_PROJECT]);

  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.getByRole("menuitem", { name: "Create note here" }).click();

  // The new note is opened, not merely created — a create that leaves you where you
  // were reads as "nothing happened".
  await expect(page).toHaveURL(/\/notes\/n-new$/);
});

test("a create failure stays in the sheet with the chosen context visible", async ({ page }) => {
  await mockTauri(
    page,
    {
      create_dashboard: () => Promise.reject(new Error("write failed")),
    },
    { list_workspace_tree: [OPEN_PROJECT] },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Create in Workspaces" }).click();

  const sheet = page.getByRole("dialog", { name: "Create in Workspaces" });
  await sheet.getByRole("button", { name: "Dashboard", exact: true }).click();
  await sheet.getByLabel("Name").fill("Roadmap pulse");
  await sheet.getByRole("button", { name: /Acme \/ Shared \/ Planning/ }).click();
  await sheet.getByRole("button", { name: "Create dashboard" }).click();

  await expect(sheet).toBeVisible();
  await expect(sheet.getByRole("alert")).toContainText("write failed");
});

test("global New creates multiple peer Workspaces even when there are no destinations", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      create_space: (args: unknown) => {
        const target = window as unknown as { __spaceCalls?: unknown[] };
        (target.__spaceCalls ??= []).push(args);
        const index = target.__spaceCalls.length;
        return { id: `p-new-${index}`, name: (args as { name: string }).name, path: `spaces/${index}`, parentId: null, locked: false, createdAt: "2026-08-26T00:00:00Z" };
      },
    },
    { list_workspace_tree: [], list_container_items: { kind: "meeting", items: [], total: 0 } },
  );
  await page.goto("/");
  await expect(page.getByText("No Workspaces yet", { exact: true })).toBeVisible();

  const createSpace = async (name: string): Promise<void> => {
    await page.getByRole("button", { name: "Create in Workspaces" }).click();
    const sheet = page.getByRole("dialog", { name: "Create in Workspaces" });
    await expect(sheet.getByRole("button", { name: "Workspace", exact: true })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await expect(sheet.getByText("Destination", { exact: true })).toHaveCount(0);
    await sheet.getByLabel("Name").fill(name);
    await sheet.getByRole("button", { name: "Create Workspace" }).click();
  };

  await createSpace("Product");
  await expect(page).toHaveURL(/\/container\/p-new-1$/);
  await createSpace("Personal");
  await expect(page).toHaveURL(/\/container\/p-new-2$/);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __spaceCalls?: unknown[] }).__spaceCalls ?? [],
      ),
    )
    .toEqual([{ name: "Product" }, { name: "Personal" }]);
});

test("a sealed container offers no way to create inside it", async ({ page }) => {
  await open(page, [SEALED_PROJECT, OPEN_PROJECT]);

  // There is no key to seal a new child with, so the backend refuses. An affordance
  // that always errors is worse than none.
  await page.getByRole("button", { name: "Actions for Clients" }).click();
  await expect(page.getByRole("menuitem", { name: /Create .* here/ })).toHaveCount(0);

  // A stale backend payload must not turn a descendant of the sealed Workspace into a
  // destination or leak its name/breadcrumb through the explicit create picker.
  await page.getByRole("button", { name: "Create in Workspaces" }).click();
  const sheet = page.getByRole("dialog", { name: "Create in Workspaces" });
  await expect(sheet).not.toContainText("Secret child");
  await expect(sheet.getByText(/Clients \/ Secret child/)).toHaveCount(0);
});

/**
 * "New note" from the sidebar's create menu is the deliberate note: it must let
 * the user say where the note lives, INCLUDING somewhere that does not exist
 * yet, without making them create the container in a separate pass first.
 */
async function openNewNoteSheet(page: Page) {
  await page.getByRole("button", { name: "Create", exact: true }).click();
  await page.getByRole("menuitem", { name: "New note" }).click();
  const sheet = page.getByRole("dialog", { name: "Create in Workspaces" });
  await expect(sheet).toBeVisible();
  await expect(
    sheet.getByRole("button", { name: "Note", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  return sheet;
}

test("New note can create the Workspace it lands in, in one pass", async ({
  page,
}) => {
  await open(page, [OPEN_PROJECT]);
  const sheet = await openNewNoteSheet(page);

  await sheet.getByRole("button", { name: "New Workspace", exact: true }).click();
  // A brand-new top-level Workspace has no parent to pick, so the destination
  // list has nothing left to ask and gets out of the way.
  await expect(sheet.getByText("Destination", { exact: true })).toHaveCount(0);

  await sheet.getByLabel("New Workspace name").fill("Product");
  await sheet.getByLabel("Name", { exact: true }).fill("Kickoff");
  await sheet.getByRole("button", { name: "Create note" }).click();

  await expect(page).toHaveURL(/\/notes\/n-new$/);
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __spaceCalls?: unknown[] }).__spaceCalls ?? [],
      ),
    )
    .toEqual([{ name: "Product" }]);
  // The note lands in the container that was just created — by the id its own
  // create returned, not in the default folder.
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __noteCalls?: unknown[] }).__noteCalls ?? [],
      ),
    )
    .toEqual([{ folderId: "p-new-1", title: "Kickoff" }]);
});

test("New note can create a folder under a chosen parent and land in it", async ({
  page,
}) => {
  await open(page, [OPEN_PROJECT]);
  const sheet = await openNewNoteSheet(page);

  await sheet.getByRole("button", { name: "New folder", exact: true }).click();
  await sheet.getByLabel("New folder name").fill("Sprint 12");

  // A new folder needs somewhere to live, so Create stays refused and the sheet
  // says what is missing — the destination list is still what answers it.
  await expect(sheet.getByRole("button", { name: "Create note" })).toBeDisabled();
  await expect(sheet).toContainText(
    "Pick below which Workspace or folder should hold the new folder.",
  );
  await sheet.getByRole("button", { name: /Acme \/ Shared \/ Planning/ }).click();
  await expect(sheet.getByRole("button", { name: "Create note" })).toBeEnabled();

  await sheet.getByLabel("Name", { exact: true }).fill("Retro");
  await sheet.getByRole("button", { name: "Create note" }).click();

  await expect(page).toHaveURL(/\/notes\/n-new$/);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __noteFolderCalls?: unknown[] })
            .__noteFolderCalls ?? [],
      ),
    )
    .toEqual([{ name: "Sprint 12", parentId: "f-team-deep" }]);
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __noteCalls?: unknown[] }).__noteCalls ?? [],
      ),
    )
    .toEqual([{ folderId: "nf-new", title: "Retro" }]);
});

test("New note still takes an existing destination, and writes nothing until Create", async ({
  page,
}) => {
  await open(page, [OPEN_PROJECT]);
  const sheet = await openNewNoteSheet(page);

  expect(
    await page.evaluate(
      () => (window as unknown as { __noteCalls?: unknown[] }).__noteCalls ?? [],
    ),
  ).toEqual([]);

  await sheet.getByRole("button", { name: /Acme \/ Shared \/ Planning/ }).click();
  await sheet.getByLabel("Name", { exact: true }).fill("Notes from today");
  await sheet.getByRole("button", { name: "Create note" }).click();

  await expect(page).toHaveURL(/\/notes\/n-new$/);
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __noteCalls?: unknown[] }).__noteCalls ?? [],
      ),
    )
    .toEqual([{ folderId: "f-team-deep", title: "Notes from today" }]);
});
