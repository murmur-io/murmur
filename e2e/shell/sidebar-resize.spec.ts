import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * The expanded sidebar's width is adjustable: grab its right edge and drag.
 * The width is clamped, survives a reload, moves `--shell-content-inset` with
 * it (so `position: fixed` views still clear the sidebar), and a double-click
 * on the edge restores the default. The handle is a keyboard-operable
 * `separator` too.
 */
async function boot(page: Page): Promise<void> {
  await mockTauri(page, {}, { list_workspace_tree: [] });
  await page.goto("/record");
  await expect(sidebar(page)).toBeVisible();
}

function sidebar(page: Page) {
  return page.getByRole("navigation", { name: "Primary navigation" });
}

function handle(page: Page) {
  return page.getByRole("separator", { name: "Resize sidebar" });
}

async function sidebarWidth(page: Page): Promise<number> {
  return Math.round((await sidebar(page).boundingBox())?.width ?? 0);
}

async function dragEdgeBy(page: Page, dx: number): Promise<void> {
  const box = await handle(page).boundingBox();
  if (!box) throw new Error("resize handle has no box");
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx / 2, y, { steps: 4 });
  await page.mouse.move(x + dx, y, { steps: 4 });
  await page.mouse.up();
}

test("dragging the sidebar's right edge changes its width and persists", async ({
  page,
}) => {
  await boot(page);
  const before = await sidebarWidth(page);
  expect(before).toBe(256);

  await dragEdgeBy(page, 80);
  await expect.poll(() => sidebarWidth(page)).toBe(before + 80);

  // The content inset follows the sidebar, or /settings would sit under it.
  const inset = await page.evaluate(() => {
    const shell = document.querySelector("app-shell") as HTMLElement;
    const probe = document.createElement("div");
    // Absolute, or the shell's flex layout shrinks the probe.
    probe.style.cssText = "position:absolute;width:var(--shell-content-inset)";
    shell.appendChild(probe);
    const w = probe.getBoundingClientRect().width;
    probe.remove();
    return Math.round(w);
  });
  const sbBox = await sidebar(page).boundingBox();
  expect(inset).toBeGreaterThanOrEqual(Math.round((sbBox?.x ?? 0) + (sbBox?.width ?? 0)));

  await page.reload();
  await expect(sidebar(page)).toBeVisible();
  await expect.poll(() => sidebarWidth(page)).toBe(before + 80);
});

test("the width is clamped to its limits", async ({ page }) => {
  await boot(page);
  // -200 keeps the pointer inside the window (webkit rejects negative x).
  await dragEdgeBy(page, -200);
  await expect.poll(() => sidebarWidth(page)).toBe(200);
  await dragEdgeBy(page, 900);
  await expect.poll(() => sidebarWidth(page)).toBe(480);
});

test("double-clicking the edge restores the default width", async ({ page }) => {
  await boot(page);
  await dragEdgeBy(page, 100);
  await expect.poll(() => sidebarWidth(page)).toBe(356);
  await handle(page).dblclick();
  await expect.poll(() => sidebarWidth(page)).toBe(256);
  await page.reload();
  await expect.poll(() => sidebarWidth(page)).toBe(256);
});

test("the edge is keyboard-operable", async ({ page }) => {
  await boot(page);
  await handle(page).focus();
  await page.keyboard.press("ArrowRight");
  await expect.poll(() => sidebarWidth(page)).toBe(272);
  await expect(handle(page)).toHaveAttribute("aria-valuenow", "272");
  await page.keyboard.press("Home");
  await expect.poll(() => sidebarWidth(page)).toBe(200);
  await page.keyboard.press("Enter");
  await expect.poll(() => sidebarWidth(page)).toBe(256);
});

test("collapsed, there is no resize handle and the rail keeps its width", async ({
  page,
}) => {
  await boot(page);
  await dragEdgeBy(page, 60);
  await page.getByRole("button", { name: /collapse sidebar/i }).click();
  await expect(handle(page)).toHaveCount(0);
  await expect.poll(() => sidebarWidth(page)).toBe(100);
  await page.getByRole("button", { name: /expand sidebar/i }).click();
  await expect.poll(() => sidebarWidth(page)).toBe(316);
});
