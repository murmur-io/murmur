import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Root-cause fix (2026-07-15): the tab-strip label must reflect a typed title
 * IMMEDIATELY, independent of whether the debounced autosave (`save_note_text`)
 * has landed yet. Before the fix, `onTitleInput` only wrote the local `title`
 * signal + scheduled the debounced save — `TabsService.setTitle(...)` was
 * called ONLY inside `saveText()`/`saveFull()` on a SUCCESSFUL save, so a save
 * that is slow (or that fails, per the sibling contention bug) left the tab
 * showing its stale/creation-time label even though the user had typed real
 * text into the editor.
 *
 * RED contract: `save_note_text` here NEVER resolves (a promise that hangs
 * forever), simulating a save that is still in flight / never lands. Against
 * the pre-fix `onTitleInput` (title sync only inside the save success path),
 * the tab label would stay "My First Note" forever since the save never
 * completes. The fix makes the tab label update on the very next paint after
 * typing, with no dependency on the save round-trip at all.
 */
test.describe("Note editor — tab-strip title updates live, independent of save completion", () => {
  test("typing a new title updates the visible tab label even while the save never resolves", async ({
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
      // A save that never settles — the tab title must update WITHOUT this
      // ever resolving, proving the sync is not gated on save completion.
      save_note_text: () => new Promise(() => {}),
    });

    await page.goto("/notes");
    await expect(page.locator(".notes-content")).toBeVisible();

    // Open "My First Note" as a real tab (drives TabsService.openNote, unlike a
    // direct page.goto to the note route, which never registers a tab).
    await page.locator(".title-btn", { hasText: "My First Note" }).click();

    const tabLabel = page.locator("mur-tab-strip .tab-item.active .tab-label");
    await expect(tabLabel).toHaveText("My First Note");

    // Retype the title.
    const titleInput = page.locator(".note-title-input");
    await expect(titleInput).toHaveValue("My First Note");
    await titleInput.fill("");
    await titleInput.type("Renamed while saving");

    // The tab label reflects the typed text RIGHT AWAY — no wait for a save
    // round-trip (which here never resolves at all).
    await expect(tabLabel).toHaveText("Renamed while saving", { timeout: 2_000 });

    expect(consoleErrors).toEqual([]);
  });
});
