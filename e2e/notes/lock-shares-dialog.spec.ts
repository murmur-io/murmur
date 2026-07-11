import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * PK-F1 — the Notes-section folder lock must run the SAME lock×shares flow as the
 * meetings tree: probe `folder_active_shares` FIRST, and if the folder still has
 * live shares open the warn/revoke dialog BEFORE calling `lock_folder`. Previously
 * `NotesHomeComponent.lockFolder` called `FoldersService.lock` directly with no
 * probe, so a shared note-folder was sealed without warning the owner.
 *
 * This spec pins the fix against the REAL FE bundle (only Tauri IPC mocked):
 *  - `folder_active_shares` returns an active share for the folder;
 *  - `lock_folder` records page-side WHETHER it was called;
 * then locks the open "Notes" folder from the Notes rail and asserts:
 *  1. the lock×shares dialog appears, and
 *  2. `lock_folder` was NOT invoked until the user confirms (Lock anyway) in the dialog.
 *
 * RED contract: on the pre-fix code the Notes lock button calls `lock_folder`
 * immediately (no dialog) → the dialog-visible assertion times out / `__lockCalled`
 * is already true before any confirm.
 */
test.describe("Notes — folder lock runs the lock×shares dialog (PK-F1)", () => {
  test("locking a shared note-folder shows the dialog BEFORE lock_folder is invoked", async ({
    page,
  }) => {
    // Layer PK-F1 overrides into the SAME Notes mock call (a second mockTauri would
    // re-install the base internals and wipe the Notes overrides).
    await mockNotes(page, {
      // The folder still has an active 1:1 user share → the flow must warn, not seal.
      folder_active_shares: () => ({ links: 0, users: 1, org: [] }),
      // Record page-side whether lock_folder ever ran (serialized override — no closures).
      lock_folder: () => {
        (window as unknown as { __lockCalled?: boolean }).__lockCalled = true;
        return null;
      },
      revoke_shares_for_folder: () => null,
    });

    await page.goto("/notes");
    await expect(page.locator(".notes-content")).toBeVisible();

    // The open "Notes" folder has a Lock control on its row.
    const lockBtn = page.getByRole("button", { name: "Lock folder" }).first();
    await expect(lockBtn).toBeVisible();
    await lockBtn.click();

    // The lock×shares dialog appears (it was NOT sealed straight away).
    const dialog = page.locator("app-lock-shares-dialog [role='dialog']");
    await expect(dialog).toBeVisible({ timeout: 5_000 });
    await expect(dialog.getByText("has active shares")).toBeVisible();

    // Critically: lock_folder must NOT have been called yet — the dialog gates it.
    expect(
      await page.evaluate(
        () => (window as unknown as { __lockCalled?: boolean }).__lockCalled ?? false,
      ),
    ).toBe(false);

    // Confirm via "Lock anyway" → NOW lock_folder fires.
    await dialog.getByRole("button", { name: "Lock anyway" }).click();
    await expect
      .poll(
        () =>
          page.evaluate(
            () =>
              (window as unknown as { __lockCalled?: boolean }).__lockCalled ??
              false,
          ),
        { timeout: 5_000 },
      )
      .toBe(true);

    // The dialog dismisses once the lock lands.
    await expect(dialog).toBeHidden();
  });
});
