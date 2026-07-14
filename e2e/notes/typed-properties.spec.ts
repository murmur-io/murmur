import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Feature C — typed note front-matter properties + folder Table/Board views.
 *
 * The schema/typed commands (`get_note_folder_schema` / `set_note_folder_schema`
 * / `list_notes_typed`) are layered as per-spec `extra` overrides on top of the
 * shared Notes mock. `nf1` ("Notes") gets a 3-field schema (a Select "Status", a
 * Checkbox "Reviewed", a Date "Due"); the locked folder `nf2` returns `[]` from
 * BOTH the schema + typed reads (backend-gated), so no typed view is offered.
 *
 * These are the runtime checks a green `ng build` can't make: the schema-driven
 * WIDGET per property row (a checkbox renders `mur-toggle`, NOT a text input),
 * the List/Table/Board switcher gated on a non-empty schema, the Table's
 * column-per-field, the Board's group-by-Status, and — critically — that toggling
 * a typed value re-serializes the note's front-matter unchanged in shape (the
 * `properties` map stays `Record<string,string>`).
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
    properties: { Status: "In progress", Reviewed: "true", Due: "2026-08-01" },
    updatedAt: 1_720_000_000_000,
    createdAt: 1_719_000_000_000,
    exportedPath: null,
    locked: false,
    shared: false,
  }),
};

test("editor renders a SCHEMA-DRIVEN widget per property (checkbox → mur-toggle, select dropdown, date input) and toggling round-trips the front-matter", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  // Capture the exact markdown the editor sends to save_note_text so we can
  // assert the front-matter round-trip stays byte-shaped.
  await mockNotes(page, {
    ...TYPED_OVERRIDES,
    ...NOTE_WITH_PROPS,
    save_note_text: (args: { id: string; title: string; markdown: string }) => {
      (window as unknown as { __lastSave?: unknown }).__lastSave = args.markdown;
      return 1_720_000_100_000;
    },
  });
  await page.goto("/notes/n1");

  // Title hydrated → the doc loaded.
  await expect(page.locator(".note-title-input")).toHaveValue("My First Note");

  // The properties bar auto-opens (it has props). Each row's KEY is present.
  await expect(page.locator(".prop-key", { hasText: "Status" })).toBeVisible();
  await expect(page.locator(".prop-key", { hasText: "Reviewed" })).toBeVisible();
  await expect(page.locator(".prop-key", { hasText: "Due" })).toBeVisible();

  // Checkbox → a mur-toggle (NOT a text input). Find the Reviewed row.
  const reviewedRow = page.locator(".prop-row", { hasText: "Reviewed" });
  await expect(reviewedRow.locator("mur-toggle")).toBeVisible();
  await expect(reviewedRow.locator('input[type="text"]')).toHaveCount(0);
  // Its switch reflects the true value.
  await expect(reviewedRow.locator('input.switch')).toBeChecked();

  // Select → a native <select> carrying the schema options + the current value.
  const statusRow = page.locator(".prop-row", { hasText: "Status" });
  await expect(statusRow.locator("select")).toBeVisible();
  await expect(statusRow.locator("select")).toHaveValue("In progress");
  await expect(statusRow.locator("select option", { hasText: "Done" })).toHaveCount(1);

  // Date → an <input type="date"> with the ISO value.
  const dueRow = page.locator(".prop-row", { hasText: "Due" });
  await expect(dueRow.locator('input[type="date"]')).toHaveValue("2026-08-01");

  // Toggle the checkbox OFF → the editor writes `Reviewed: false` back into the
  // SAME front-matter block (round-trip: still `key: value` scalars, no shape drift).
  await reviewedRow.locator("input.switch").click();
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __lastSave?: string }).__lastSave ?? "",
      ),
    )
    .toContain("Reviewed: false");
  const saved = await page.evaluate(
    () => (window as unknown as { __lastSave?: string }).__lastSave ?? "",
  );
  // The other typed properties survived unchanged (no data loss on a typed edit).
  expect(saved).toContain("Status: In progress");
  expect(saved).toContain("Due: 2026-08-01");
  expect(saved).toContain("tags: [idea]");
  // Still a single leading --- … --- YAML block (the byte-exact serializer path).
  expect(saved.startsWith("---\n")).toBeTruthy();

  expect(consoleErrors).toEqual([]);
});

test("notes home offers List/Table/Board only for a folder WITH a schema; Table shows a column per field; Board groups by Status", async ({
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

  // "All notes" (no folder) → NO switcher.
  await expect(page.locator(".view-switcher")).toHaveCount(0);

  // Select the "Notes" (nf1) folder in the sidebar tree → its schema loads.
  await page
    .locator("app-notes-sidebar-tree mur-tree-row .row-label", { hasText: "Notes" })
    .first()
    .click();

  // The switcher now appears (nf1 has a non-empty schema).
  const switcher = page.locator(".view-switcher");
  await expect(switcher).toBeVisible();
  await expect(switcher.getByRole("button", { name: "Table" })).toBeVisible();
  await expect(switcher.getByRole("button", { name: "Board" })).toBeVisible();

  // Switch to TABLE → a column per schema field + Title + Updated.
  await switcher.getByRole("button", { name: "Table" }).click();
  const table = page.locator("app-notes-table-view");
  await expect(table).toBeVisible();
  await expect(table.locator("thead th", { hasText: "Title" })).toBeVisible();
  await expect(table.locator("thead th", { hasText: "Status" })).toBeVisible();
  await expect(table.locator("thead th", { hasText: "Reviewed" })).toBeVisible();
  await expect(table.locator("thead th", { hasText: "Due" })).toBeVisible();
  // A select value renders as a pill; a checkbox as a glyph (n1 Reviewed=true).
  await expect(table.locator(".select-pill", { hasText: "In progress" })).toBeVisible();
  await expect(table.locator(".check-glyph").first()).toBeVisible();

  // Switch to BOARD → grouped by the Select field "Status": one column per option.
  await switcher.getByRole("button", { name: "Board" }).click();
  const board = page.locator("app-notes-board-view");
  await expect(board).toBeVisible();
  await expect(board.locator(".board-col-title", { hasText: "Todo" })).toBeVisible();
  await expect(board.locator(".board-col-title", { hasText: "In progress" })).toBeVisible();
  await expect(board.locator(".board-col-title", { hasText: "Done" })).toBeVisible();
  // n1 (In progress) card is under the right column.
  await expect(board.getByText("My First Note")).toBeVisible();

  expect(consoleErrors).toEqual([]);
});

test("a LOCKED folder (schema [] / rows []) offers NO typed view — only the lock gate / list", async ({
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

  // Select the locked "Work" folder (nf2) → schema gated to [].
  await page
    .locator("app-notes-sidebar-tree mur-tree-row .row-label", { hasText: "Work" })
    .first()
    .click();

  // NO switcher (empty schema), NO table/board view.
  await expect(page.locator(".view-switcher")).toHaveCount(0);
  await expect(page.locator("app-notes-table-view")).toHaveCount(0);
  await expect(page.locator("app-notes-board-view")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});
