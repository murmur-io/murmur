import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * The global rail collapses to icons (the default) and expands to show each
 * destination's label beside its glyph.
 *
 * The labels are `aria-hidden`, because every rail control already carries an
 * `aria-label` and revealing the text must not make a screen reader announce
 * the destination twice — so these assertions go through `.rail-label` rather
 * than `getByRole`, which by design cannot see them.
 */
async function boot(page: Page): Promise<void> {
  await mockTauri(page, {}, { list_workspace_tree: [] });
  await page.goto("/record");
}

function rail(page: Page) {
  return page.getByRole("navigation", { name: "Global navigation" });
}

test("the rail starts collapsed to icons only", async ({ page }) => {
  await boot(page);

  const nav = rail(page);
  await expect(nav).toBeVisible();
  await expect(nav.getByRole("button", { name: "Expand sidebar" })).toBeVisible();

  // Every label exists in the DOM but none of them takes up space.
  await expect(nav.locator(".rail-label")).toHaveCount(8);
  await expect(nav.locator(".rail-label").first()).toBeHidden();

  const box = await nav.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.width).toBeLessThanOrEqual(74);
});

test("expanding reveals the labels and widens the rail", async ({ page }) => {
  await boot(page);
  const nav = rail(page);
  const collapsed = await nav.boundingBox();

  await nav.getByRole("button", { name: "Expand sidebar" }).click();

  await expect(nav.getByText("Capture", { exact: true })).toBeVisible();
  await expect(nav.getByText("Workspaces", { exact: true })).toBeVisible();
  await expect(nav.getByText("Settings", { exact: true })).toBeVisible();

  const expanded = await nav.boundingBox();
  expect(expanded).not.toBeNull();
  expect(expanded!.width).toBeGreaterThan(collapsed!.width + 100);

  // The control now offers the opposite action.
  await expect(nav.getByRole("button", { name: "Collapse sidebar" })).toBeVisible();
  await expect(nav.getByRole("button", { name: "Collapse sidebar" })).toHaveAttribute(
    "aria-expanded",
    "true",
  );
});

test("the choice survives a reload, and collapsing restores the icon rail", async ({
  page,
}) => {
  await boot(page);
  const nav = rail(page);
  await nav.getByRole("button", { name: "Expand sidebar" }).click();
  await expect(nav.getByText("Capture", { exact: true })).toBeVisible();

  await page.reload();
  const reloaded = rail(page);
  await expect(reloaded.getByText("Capture", { exact: true })).toBeVisible();

  await reloaded.getByRole("button", { name: "Collapse sidebar" }).click();
  await expect(reloaded.getByText("Capture", { exact: true })).toBeHidden();

  const box = await reloaded.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.width).toBeLessThanOrEqual(74);

  // And the collapsed state is what a further reload restores.
  await page.reload();
  await expect(
    rail(page).getByRole("button", { name: "Expand sidebar" }),
  ).toBeVisible();
});
