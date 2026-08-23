import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const OPEN_PROJECT = {
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
};

const SEALED_PROJECT = {
  ...OPEN_PROJECT,
  id: "p-sealed",
  name: "Klienci",
  locked: true,
  unlocked: false,
};

async function open(page: Page, forest: unknown[]): Promise<void> {
  await mockTauri(
    page,
    {
      create_note: () => "n-new",
      create_folder: () => ({
        id: "f-new",
        name: "Nowy folder",
        path: "Acme/Nowy folder",
        parentId: "p-acme",
        locked: false,
        createdAt: "2026-08-23T00:00:00Z",
      }),
    },
    { list_workspace_tree: forest },
  );
  await page.goto("/");
  await expect(page.getByRole("tree", { name: "Hierarchia obszaru roboczego" })).toBeVisible();
}

test("a note can be created into a container and opens straight away", async ({ page }) => {
  await open(page, [OPEN_PROJECT]);

  await page.getByRole("button", { name: "Dodaj do Acme" }).click();
  await page.getByRole("menuitem", { name: "Nowa notatka" }).click();

  // The new note is opened, not merely created — a create that leaves you where you
  // were reads as "nothing happened".
  await expect(page).toHaveURL(/\/notes\/n-new$/);
});

test("a sealed container offers no way to create inside it", async ({ page }) => {
  await open(page, [SEALED_PROJECT]);

  // There is no key to seal a new child with, so the backend refuses. An affordance
  // that always errors is worse than none.
  await expect(page.getByRole("button", { name: "Dodaj do Klienci" })).toHaveCount(0);
});
