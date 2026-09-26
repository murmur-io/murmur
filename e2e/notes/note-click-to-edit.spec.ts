import { test, expect, type Page } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Click the note column's blank margin — inside the scroll region, outside the
 * document surface. Found in-page so it tracks the measured column width.
 */
async function clickAway(page: Page): Promise<void> {
  const point = await page.evaluate(() => {
    const region = document.querySelector(".editor-body") as HTMLElement;
    const doc = document.querySelector("app-note-document") as HTMLElement;
    const r = region.getBoundingClientRect();
    for (let x = r.left + 2; x < r.right - 20; x += 8) {
      for (let y = r.top + 40; y < r.bottom - 10; y += 40) {
        const hit = document.elementFromPoint(x, y);
        if (hit && region.contains(hit) && !doc.contains(hit)) {
          return { x, y };
        }
      }
    }
    return null;
  });
  expect(point, "a blank spot beside the note document").not.toBeNull();
  await page.mouse.click(point!.x, point!.y);
}

/**
 * Click-to-edit / click-away-to-preview. A click on the rendered note flips it
 * into Edit with the caret near the clicked text; a click outside the note
 * surface returns it to Preview. Links, the toggle, and a note with no body keep
 * their own behaviour.
 */
test("clicking away from the note returns to Preview, clicking the note returns to Edit", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (err) => errors.push(String(err)));

  await mockNotes(page);
  await page.goto("/notes/n1");
  const doc = page.locator("app-note-document");
  const body = page.locator(".body-area");
  await expect(doc).toHaveAttribute("data-mode", "edit");

  // Clicking inside the note (textarea, title) keeps Edit.
  await body.click();
  await page.locator(".note-title-input").click();
  await expect(doc).toHaveAttribute("data-mode", "edit");

  // The note's own header chrome keeps Edit (it acts on the note being edited).
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("button", { name: "Close menu" }).click();
  await expect(doc).toHaveAttribute("data-mode", "edit");

  // Click away (the column's blank margin beside the document) → Preview.
  await clickAway(page);
  await expect(doc).toHaveAttribute("data-mode", "preview");
  await expect(page.locator(".note-preview")).toBeVisible();

  // Clicking outside again while in Preview stays in Preview.
  await clickAway(page);
  await expect(doc).toHaveAttribute("data-mode", "preview");

  // Click the rendered body → Edit, textarea focused, caret inside the clicked text.
  await page.locator(".note-preview").getByText(/Some body text to select/).click();
  await expect(doc).toHaveAttribute("data-mode", "edit");
  await expect(body).toBeFocused();
  const caret = await body.evaluate((el: HTMLTextAreaElement) => ({
    at: el.selectionStart,
    collapsed: el.selectionStart === el.selectionEnd,
    start: el.value.indexOf("Some body text to select."),
  }));
  expect(caret.collapsed).toBe(true);
  expect(caret.at).toBeGreaterThanOrEqual(caret.start);
  expect(caret.at).toBeLessThanOrEqual(caret.start + "Some body text to select.".length);

  // Away again, then the rendered title → Edit with the title input focused.
  await clickAway(page);
  await expect(doc).toHaveAttribute("data-mode", "preview");
  await page.locator(".document-title").click();
  await expect(doc).toHaveAttribute("data-mode", "edit");
  await expect(page.locator(".note-title-input")).toBeFocused();

  // The explicit toggle still works in both directions.
  const seg = page.getByRole("group", { name: "Edit or preview" });
  await seg.getByRole("button", { name: "Preview", exact: true }).click();
  await expect(doc).toHaveAttribute("data-mode", "preview");
  await seg.getByRole("button", { name: "Edit", exact: true }).click();
  await expect(doc).toHaveAttribute("data-mode", "edit");

  expect(errors).toEqual([]);
});

test("a note with no body stays in Edit on click-away", async ({ page }) => {
  await mockNotes(page, {
    get_note: () => ({
      id: "n1",
      title: "",
      folderId: null,
      markdown: "",
      tags: [],
      updatedAt: 1_720_000_000_000,
      createdAt: 1_720_000_000_000,
      exportedPath: null,
      locked: false,
      shared: false,
    }),
  });
  await page.goto("/notes/n1");
  const doc = page.locator("app-note-document");
  await expect(doc).toHaveAttribute("data-mode", "edit");
  await clickAway(page);
  await expect(doc).toHaveAttribute("data-mode", "edit");
});

test("returning to the note while the click-away save is still running keeps Edit", async ({
  page,
}) => {
  await mockNotes(page, {
    // The real update_note_doc embeds behind the heavy-inference gate: hold it open.
    update_note_doc: (a: { id: string; title: string; markdown: string }) =>
      new Promise((resolve) => {
        (window as any).__releaseSave = () =>
          resolve({
            id: a.id,
            title: a.title,
            folderId: "nf1",
            markdown: a.markdown,
            tags: [],
            updatedAt: 1_720_000_000_001,
            createdAt: 1_720_000_000_000,
            exportedPath: null,
            locked: false,
            shared: false,
          });
      }),
  });
  await page.goto("/notes/n1");
  const doc = page.locator("app-note-document");
  const body = page.locator(".body-area");
  await body.click();
  await page.keyboard.type(" edit");

  await clickAway(page);
  await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseSave)).toBe("function");
  await body.click();
  await page.keyboard.type(" more");
  await page.evaluate(() => (window as any).__releaseSave());

  // Give the settled save a chance to (wrongly) flip the mode.
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  await expect(doc).toHaveAttribute("data-mode", "edit");
  await expect(body).toHaveValue(/ edit/);
  await expect(body).toHaveValue(/ more/);
});
