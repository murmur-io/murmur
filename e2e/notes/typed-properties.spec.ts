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
 * Views bar (`app-notes-view-switcher`, Board dropped). The EDITOR's typed
 * property widgets (test 1 below) are unchanged and still shipped.
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
  await expect(page.getByText("This container is locked")).toBeVisible();
  // No view controls and no counts: the backend refuses to describe a sealed container, and
  // "0" would be a claim about contents nobody is entitled to read.
  await expect(page.locator("app-notes-view-switcher")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});
