import { test, expect } from "@playwright/test";
import { enterEditMode, mockNotes } from "./mock-invoke";

/**
 * Note edit-lock — the header padlock makes a note read-only, persisted per note
 * (`documents.edit_locked` via `set_note_edit_locked`).
 *
 * The mock keeps the flag page-side and `get_note` reflects it, so a reload proves
 * the editor HYDRATES the lock rather than holding it in component state. It also
 * records the invoke order: locking must flush the pending edit BEFORE the lock
 * lands, or the last keystrokes are lost behind a read-only view.
 */
test("the padlock locks a note read-only, flushes the pending edit first, survives a reload and unlocks again", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, {
    get_note: (args: { id: string }) => {
      const w = window as unknown as { __editLocked?: boolean };
      return {
        id: args.id,
        title: "My First Note",
        folderId: "nf1",
        markdown: "# Heading\n\nSome body text to select.",
        tags: ["idea"],
        updatedAt: 1_720_000_000_000,
        createdAt: 1_719_000_000_000,
        exportedPath: null,
        locked: false,
        shared: false,
        editLocked: w.__editLocked === true,
      };
    },
    save_note_text: (args: { markdown: string }) => {
      const w = window as unknown as { __calls?: string[] };
      (w.__calls ??= []).push(`save_note_text:${args.markdown.includes("typed before lock")}`);
      return 1_720_000_100_000;
    },
    update_note_doc: (args: { id: string; title: string; markdown: string }) => {
      const w = window as unknown as { __calls?: string[] };
      (w.__calls ??= []).push(`update_note_doc:${args.markdown.includes("typed before lock")}`);
      return {
        id: args.id,
        title: args.title,
        folderId: "nf1",
        markdown: args.markdown,
        tags: ["idea"],
        updatedAt: 1_720_000_100_000,
        createdAt: 1_719_000_000_000,
        exportedPath: null,
        locked: false,
        shared: false,
        editLocked: false,
      };
    },
    set_note_edit_locked: (args: { id: string; locked: boolean }) => {
      const w = window as unknown as { __calls?: string[]; __editLocked?: boolean };
      (w.__calls ??= []).push(`set_note_edit_locked:${args.locked}`);
      w.__editLocked = args.locked;
      // Persist across the reload below (page-side state is otherwise wiped).
      sessionStorage.setItem("__editLocked", String(args.locked));
      return {
        id: args.id,
        title: "My First Note",
        folderId: "nf1",
        markdown: "# Heading\n\nSome body text to select.",
        tags: ["idea"],
        updatedAt: 1_720_000_000_000,
        createdAt: 1_719_000_000_000,
        exportedPath: null,
        locked: false,
        shared: false,
        editLocked: args.locked,
      };
    },
  });
  await page.addInitScript(() => {
    (window as unknown as { __editLocked?: boolean }).__editLocked =
      sessionStorage.getItem("__editLocked") === "true";
  });

  await page.goto("/notes/n1");
  await enterEditMode(page);

  const lockBtn = page.getByRole("button", { name: "Lock editing" });
  await expect(lockBtn).toHaveAttribute("aria-pressed", "false");

  // Type, then lock BEFORE the 600ms debounce fires — the edit must still land first.
  await page.locator(".body-area").click();
  await page.keyboard.type(" typed before lock");
  await lockBtn.click();

  const unlockBtn = page.getByRole("button", { name: "Unlock editing" });
  await expect(unlockBtn).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".body-area")).toHaveCount(0);
  await expect(page.locator(".note-title-input")).toHaveCount(0);
  await expect(page.locator("h1.document-title")).toHaveText("My First Note");
  await expect(page.getByRole("button", { name: "Edit", exact: true })).toBeDisabled();

  const calls = await page.evaluate(
    () => (window as unknown as { __calls?: string[] }).__calls ?? [],
  );
  const lockIdx = calls.indexOf("set_note_edit_locked:true");
  const saveIdx = calls.findIndex((c) => c.endsWith(":true") && !c.startsWith("set_"));
  expect(lockIdx, `invoke order: ${calls.join(", ")}`).toBeGreaterThan(-1);
  expect(saveIdx, `the typed text was saved before the lock: ${calls.join(", ")}`).toBeGreaterThan(-1);
  expect(saveIdx).toBeLessThan(lockIdx);

  // Persisted: a fresh load opens read-only.
  await page.reload();
  await expect(page.getByRole("button", { name: "Unlock editing" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.locator(".body-area")).toHaveCount(0);

  // Unlocking brings the editor back.
  await page.getByRole("button", { name: "Unlock editing" }).click();
  await expect(page.getByRole("button", { name: "Lock editing" })).toHaveAttribute(
    "aria-pressed",
    "false",
  );
  await enterEditMode(page);
  await expect(page.locator(".body-area")).toBeEditable();

  expect(consoleErrors).toEqual([]);
});
