import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Feature C — typed note front-matter properties (the NOTE-EDITOR side) + the
 * Notes list SAVED VIEWS switcher.
 *
 * The schema/typed commands (`get_note_folder_schema` / `set_note_folder_schema`
 * / `list_notes_typed`) are layered as per-spec `extra` overrides on top of the
 * shared Notes mock. `nf1` ("Notes") gets a 3-field schema (a Select "Status", a
 * Checkbox "Reviewed", a Date "Due"); the locked folder `nf2` returns `[]`.
 *
 * NOTE (2026-07-14): the notes-home per-folder List/Table/Board VIEW MODE (the
 * old `mur-segmented` schema-gated switcher + `app-notes-table-view` /
 * `app-notes-board-view`) was REMOVED and replaced by the Meetings-style Saved
 * Views bar (`app-notes-view-switcher`, Board dropped).
 *
 * NOTE (2026-09-13): the EDITOR's Properties card and its typed widgets were removed
 * at the user's request. Test 1 no longer drives widgets — it pins the invariant that
 * survived them: a note's front-matter still round-trips through a save untouched, so
 * nothing is stripped from the exported .md. `TYPED_OVERRIDES` stays for tests 2-3
 * (the Saved Views bar and the lock gate), which never touched the editor.
 */

/** The schema + typed-row overrides shared by these specs. */
const TYPED_OVERRIDES = {
  get_note_folder_schema: (args: { folderId: string }) => {
    if (args.folderId === "nf1") {
      return [
        { key: "Status", kind: "select", options: ["Todo", "In progress", "Done"] },
        { key: "Reviewed", kind: "checkbox", options: [] },
        { key: "Due", kind: "date", options: [] },
      ];
    }
    // nf2 is locked → gated to [] (no typed view).
    return [];
  },
  set_note_folder_schema: () => null,
  list_notes_typed: (args: { folderId: string }) => {
    if (args.folderId === "nf1") {
      return [
        {
          id: "n1",
          title: "My First Note",
          folderId: "nf1",
          values: {
            Status: { kind: "select", value: "In progress" },
            Reviewed: { kind: "checkbox", value: true },
            Due: { kind: "date", value: "2026-08-01" },
          },
          tags: ["idea"],
          updatedAt: 1_720_000_000_000,
        },
        {
          id: "n2",
          title: "Weekly plan",
          folderId: "nf1",
          values: {
            Status: { kind: "select", value: "Done" },
            Reviewed: { kind: "checkbox", value: false },
          },
          tags: [],
          updatedAt: 1_720_100_000_000,
        },
        {
          id: "n3",
          title: "Backlog idea",
          folderId: "nf1",
          values: { Status: { kind: "select", value: "Todo" } },
          tags: [],
          updatedAt: 1_720_200_000_000,
        },
      ];
    }
    return [];
  },
};

/** A `get_note` for n1 carrying the three typed properties in its front-matter. */
const NOTE_WITH_PROPS = {
  get_note: (args: { id: string }) => ({
    id: args.id,
    title: "My First Note",
    folderId: "nf1",
    markdown:
      "---\ntags: [idea]\nStatus: In progress\nReviewed: true\nDue: 2026-08-01\n---\n\n# Heading\n\nSome body text to select.",
    tags: ["idea"],
    updatedAt: 1_720_000_000_000,
    createdAt: 1_719_000_000_000,
    exportedPath: null,
    locked: false,
    shared: false,
  }),
};

test("a note's front-matter survives a BODY edit untouched (no Properties UI)", async ({
  page,
}) => {
  // REPLACES the schema-driven-widget test deleted on 2026-09-13 with the editor's
  // Properties card. That test was the only guard that a save re-emits the note's
  // YAML block intact, and the card's removal makes that MORE important, not less:
  // nothing in the UI shows front-matter any more, so a regression that silently
  // dropped it would reach the user's Obsidian vault invisibly — and destructively,
  // since the exported .md is overwritten.
  //
  // Same invariant, asserted through the surface that still exists: load a note whose
  // markdown carries tags + typed properties, edit only the BODY, and require every
  // front-matter line back in the saved markdown.
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, {
    ...NOTE_WITH_PROPS,
    save_note_text: (args: { id: string; title: string; markdown: string }) => {
      (window as unknown as { __lastSave?: unknown }).__lastSave = args.markdown;
      return 1_720_000_100_000;
    },
  });
  await page.goto("/notes/n1");
  await expect(page.locator(".note-title-input")).toHaveValue("My First Note");

  // The card is GONE — the point of the change.
  await expect(page.locator(".props")).toHaveCount(0);
  await expect(page.locator(".prop-row")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "+ Add property" })).toHaveCount(0);

  // A note WITH a body starts in Preview (see note-editor.spec.ts "a note with a
  // body starts in Preview"), and Preview renders no textarea — so switch to Edit
  // first or the body locator never resolves.
  await page
    .getByRole("group", { name: "Edit or preview" })
    .getByRole("button", { name: "Edit", exact: true })
    .click();

  // Edit the BODY only.
  const body = page.getByRole("textbox", { name: "Note body" });
  await expect(body).toBeVisible();
  await body.click();
  await body.pressSequentially(" edited");

  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __lastSave?: string }).__lastSave ?? "",
      ),
    )
    .toContain("edited");

  const saved = await page.evaluate(
    () => (window as unknown as { __lastSave?: string }).__lastSave ?? "",
  );
  // Every front-matter line the note arrived with is still there, in one leading
  // YAML block. This is the assertion that fails if the removal ever grows teeth it
  // should not have.
  expect(
    saved.startsWith(
      "---\ntags: [idea]\nStatus: In progress\nReviewed: true\nDue: 2026-08-01\n---\n\n",
    ),
  ).toBeTruthy();
  expect(saved).toContain("tags: [idea]");
  expect(saved).toContain("Status: In progress");
  expect(saved).toContain("Reviewed: true");
  expect(saved).toContain("Due: 2026-08-01");

  expect(consoleErrors).toEqual([]);
});

test("notes home shows the Saved Views bar (List default + add) on a folder scope, Board is gone", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, TYPED_OVERRIDES);
  await page.goto("/notes");
  await expect(page.locator(".notes-content")).toBeVisible();

  // The Saved Views bar (the Meetings-style switcher) shows on the "All notes"
  // scope — a "List" default tab + a "Save a new view" (+) button, no Board.
  const switcher = page.locator("app-notes-view-switcher");
  await expect(switcher).toBeVisible();
  await expect(switcher.getByRole("tab", { name: "List" })).toBeVisible();
  await expect(
    switcher.getByRole("button", { name: "Save a new view" }),
  ).toBeVisible();
  // Board was removed — no "As board" / "Board" affordance anywhere in the bar.
  await expect(switcher.getByRole("button", { name: /board/i })).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});

test("a LOCKED folder shows the lock gate and hides the Saved Views bar", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, TYPED_OVERRIDES);
  await page.goto("/notes");
  await expect(page.locator(".notes-content")).toBeVisible();

  // Select the locked "Work" folder (nf2).
  // Opening a folder now lands on ITS OWN view — the hierarchy replaced the per-type trees,
  // and only they ever set the Notes list's folder filter. So the property under test moved
  // with the destination: a sealed container must present itself as locked and disclose
  // nothing about what it holds, rather than showing a view of its contents.
  await expect(page).toHaveURL(/\/notes$/);
  await expect(page.locator(".notes-content")).toBeVisible();

  // The Workspaces tree is a section of the ONE sidebar now, rather than a
  // separate "Workspaces sidebar" panel opened from a rail button.
  const spacesSidebar = page.getByRole("navigation", {
    name: "Primary navigation",
  });
  await expect(spacesSidebar).toBeVisible();
  await spacesSidebar
    .getByRole("button", { name: "Expand Workspace" })
    .click();
  await spacesSidebar
    .getByRole("button", { name: "Work", exact: true })
    .click();

  await expect(page).toHaveURL(/\/container\/nf2$/);
  // "folder", not "container": the code's word for "a Workspace or a folder" used to leak into
  // this state, where it named nothing the user ever created. `core/hierarchy-vocabulary.ts`
  // is the one source now — and this assertion failing on the rename is what proved the old
  // word really was on screen rather than only in the source.
  await expect(page.getByText("This folder is locked")).toBeVisible();
  // No view controls and no counts: the backend refuses to describe a sealed container, and
  // "0" would be a claim about contents nobody is entitled to read.
  await expect(page.locator("app-notes-view-switcher")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});

test("body edits preserve opaque CRLF YAML and newly pasted front matter", async ({ page }) => {
  await mockNotes(page, {
    get_note: (args: { id: string }) => ({
      id: args.id, title: "Opaque YAML", folderId: "nf1", tags: ["idea"],
      markdown: "\uFEFF---\r\n# keep comment\r\ntags: [idea]\r\ncustom: 'a:b'\r\nnested:\r\n  keys: [one, two]\r\nempty:\r\n---\r\n\r\n\r\nOriginal body",
      updatedAt: 1720000000000, createdAt: 1719000000000, exportedPath: null, locked: false, shared: false,
    }),
    save_note_text: (args: { markdown: string }) => {
      (window as unknown as { __opaqueSave: string }).__opaqueSave = args.markdown;
      return 1720000001000;
    },
  });
  await page.goto("/notes/n1");
  const body = page.getByRole("textbox", { name: "Note body", exact: true });
  await expect(body).toHaveValue("Original body");
  await body.fill("Edited body");
  await expect.poll(() => page.evaluate(() => (window as unknown as { __opaqueSave: string }).__opaqueSave)).toBe(
    "\uFEFF---\r\n# keep comment\r\ntags: [idea]\r\ncustom: 'a:b'\r\nnested:\r\n  keys: [one, two]\r\nempty:\r\n---\r\n\r\n\r\nEdited body",
  );
});

test("newly entered YAML in a body-only note survives subsequent typing", async ({ page }) => {
  await mockNotes(page, {
    get_note: (args: { id: string }) => ({
      id: args.id, title: "Fresh YAML", folderId: "nf1", tags: [], markdown: "",
      updatedAt: 1720000000000, createdAt: 1719000000000, exportedPath: null, locked: false, shared: false,
    }),
    save_note_text: (args: { markdown: string }) => {
      (window as unknown as { __opaqueSave: string }).__opaqueSave = args.markdown;
      return 1720000001000;
    },
  });
  await page.goto("/notes/n1");
  const body = page.getByRole("textbox", { name: "Note body", exact: true });
  await body.fill("---\ncustom: keep\n---\nBody");
  await body.press("End");
  await body.pressSequentially(" changed");
  await expect.poll(() => page.evaluate(() => (window as unknown as { __opaqueSave: string }).__opaqueSave)).toBe("---\ncustom: keep\n---\nBody changed");
});
