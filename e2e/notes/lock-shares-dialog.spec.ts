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
      lock_folder_allow_remote_access: () => {
        (window as unknown as { __overrideLockCalled?: boolean }).__overrideLockCalled = true;
        return null;
      },
      revoke_shares_for_folder: () => null,
    });

    await page.goto("/notes");
    await expect(page.locator(".notes-content")).toBeVisible();

    // The hierarchy sits beside the mounted Notes list. Its plaintext stays
    // visible until the user actually confirms a lock.
    await expect(page).toHaveURL(/\/notes$/);
    await expect(page.locator(".notes-content")).toBeVisible();
    await expect(page.getByText("My First Note", { exact: true })).toBeVisible();

    // The Workspaces tree is a section of the ONE sidebar now, rather than a
    // separate "Workspaces sidebar" panel opened from a rail button.
    const spacesSidebar = page.getByRole("navigation", {
      name: "Primary navigation",
    });
    await expect(spacesSidebar).toBeVisible();
    await spacesSidebar
      .getByRole("button", { name: "Expand Workspace" })
      .click();
    // Reached by FOCUS, not a pointer. A trailing row control sits at the rail's right edge,
    // where a click can land on the rail instead once the tree is long enough to scroll — the
    // failure is engine- and layout-dependent, and passes locally while failing on one CI
    // lane. Keyboard activation is immune to whatever is painted on top.
    await spacesSidebar
      .getByRole("button", { name: "Actions for Notes" })
      .focus();
    await page.keyboard.press("Enter");
    const lockBtn = page.getByRole("menuitem", { name: /^Lock (folder|project)/ });
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

    // Confirm via "Lock anyway" → only the explicit retain-remote-access IPC fires.
    await dialog.getByRole("button", { name: "Lock anyway" }).click();
    await expect
      .poll(
        () =>
          page.evaluate(
            () =>
              (window as unknown as { __overrideLockCalled?: boolean }).__overrideLockCalled ??
              false,
          ),
        { timeout: 5_000 },
      )
      .toBe(true);
    expect(
      await page.evaluate(
        () => (window as unknown as { __lockCalled?: boolean }).__lockCalled ?? false,
      ),
    ).toBe(false);

    // The dialog dismisses once the lock lands.
    await expect(dialog).toBeHidden();
  });
});

test.describe("Notes — durable share revocation pending", () => {
  test("a failed retry stays visible and explains why delete or lock is paused", async ({
    page,
  }) => {
    await mockNotes(page, {
      account_status: () => ({
        loggedIn: true,
        email: "owner@example.test",
        unlockedForSharing: true,
        shareConsented: true,
        serverConfigured: true,
        biometricUnlockAvailable: false,
      }),
      list_my_shares: () => [
        {
          shareId: "pending-link",
          title: "My First Note",
          locked: false,
          rev: 1,
          createdAt: "2026-08-20T12:00:00Z",
          expiresAt: null,
          revoked: false,
          revokePending: true,
          downloadCount: 0,
          meetingId: null,
          documentId: "n1",
          maxDownloads: null,
          mode: "link",
        },
      ],
      revoke_share: () => {
        (window as unknown as { __revokeRetryCalled?: boolean }).__revokeRetryCalled = true;
        throw new Error("remote deletion remains unproven");
      },
    });

    await page.goto("/notes/n1");
    await page.getByRole("button", { name: "More actions" }).click();
    await page.getByRole("menuitem", { name: "Share…" }).click();

    const modal = page.getByRole("dialog", { name: "Share this note" });
    await expect(modal).toBeVisible();
    await expect(modal.getByText("Revocation pending")).toBeVisible();
    await expect(
      modal.getByText(
        "Murmur is safely closing an interrupted share. Local delete or lock stays paused until cleanup succeeds.",
      ),
    ).toBeVisible();

    await modal.getByRole("button", { name: "Retry" }).click();
    await expect.poll(() => page.evaluate(
      () => (window as unknown as { __revokeRetryCalled?: boolean }).__revokeRetryCalled ?? false,
    )).toBe(true);
    await expect(modal.getByText("Revocation pending")).toBeVisible();
    await expect(modal.getByRole("button", { name: "Retry" })).toBeVisible();
  });
});
