import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Regression for a stale save response racing a global relock. The folder starts sealed but
 * session-unlocked, `update_note_doc` is held in flight, and Lock all synchronously masks the
 * editor. A late pre-lock DTO must not flip `locked` back to false or restore the plaintext title
 * in the tab strip.
 */
test("a late full-save response cannot unmask a note after Lock all", async ({
  page,
}) => {
  await mockNotes(page, {
    list_folders: () => {
      const relocked = Boolean((window as any).__noteRelocked);
      return [
        {
          id: "nf1",
          name: "Notes",
          parentId: null,
          noteCount: 1,
          locked: true,
          unlocked: !relocked,
          children: [],
        },
      ];
    },
    relock_all: () => {
      (window as any).__noteRelocked = true;
      return null;
    },
    get_note: (args: { id: string }) => {
      if ((window as any).__noteRelocked) {
        return {
          id: args.id,
          title: "🔒 Locked",
          folderId: "nf1",
          markdown: "",
          tags: [],
          properties: {},
          updatedAt: Date.now(),
          createdAt: 1_719_000_000_000,
          exportedPath: null,
          locked: true,
          shared: false,
        };
      }
      return {
        id: args.id,
        title: "My First Note",
        folderId: "nf1",
        markdown: "# Heading\n\nSecret body",
        tags: [],
        properties: {},
        updatedAt: 1_720_000_000_000,
        createdAt: 1_719_000_000_000,
        exportedPath: null,
        locked: false,
        shared: false,
      };
    },
    save_note_text: () => Date.now(),
    update_note_doc: (args: { id: string; title: string; markdown: string }) =>
      new Promise((resolve) => {
        (window as any).__resolveLateNoteSave = () =>
          resolve({
            id: args.id,
            title: args.title,
            folderId: "nf1",
            markdown: args.markdown,
            tags: [],
            properties: {},
            updatedAt: Date.now(),
            createdAt: 1_719_000_000_000,
            exportedPath: null,
            locked: false,
            shared: false,
          });
      }),
  });

  await page.goto("/notes");
  await page.locator(".title-btn", { hasText: "My First Note" }).click();

  const tabLabel = page.locator("mur-tab-strip .tab-item.active .tab-label");
  await page.locator(".note-title-input").fill("Secret title before relock");
  await page
    .getByRole("button", { name: "Preview" })
    .click({ noWaitAfter: true });
  await page.waitForFunction(
    () => typeof (window as any).__resolveLateNoteSave === "function",
  );

  await page
    .getByRole("button", { name: /Re-seal all 1 unlocked folder now/i })
    .click();
  await expect(page.getByText(/locked folder/i)).toBeVisible();
  await expect(tabLabel).toHaveText("🔒 Locked");

  await page.evaluate(() => (window as any).__resolveLateNoteSave());
  await page.waitForTimeout(100);

  await expect(page.getByText(/locked folder/i)).toBeVisible();
  await expect(page.locator(".note-title-input")).toHaveCount(0);
  await expect(tabLabel).toHaveText("🔒 Locked");
  await expect(page.getByText("Secret title before relock")).toHaveCount(0);
});
