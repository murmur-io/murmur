import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Shared Brain v1 — FE smoke over the new org surfaces against the spec DTO
 * contract with a mocked Tauri IPC (no Rust core). Each test drives one surface
 * to a visible state, asserts the load-bearing bits render, and screenshots it
 * so the PNGs can be eyeballed. NOT a proof of end-to-end behavior (no OCK / no
 * server) — a render + no-console-error smoke.
 *
 * The overrides run PAGE-SIDE (serialized to strings), so they must be
 * self-contained — no closures over test scope.
 */

// A signed-in, sharing-ready account so the share panel opens past its gate.
const ACCOUNT_STATUS = () => ({
  loggedIn: true,
  email: "you@example.com",
  unlockedForSharing: true,
  shareConsented: true,
  serverConfigured: true,
  biometricUnlockAvailable: true,
});

// The user's orgs (multi-org): one they own, one they were invited into.
const ORG_STATUSES = () => [
  {
    orgId: "org-1",
    name: "Acme Inc.",
    role: "owner",
    memberCount: 3,
    consented: true,
    lastSeq: 42,
    itemCount: 12,
    receivedCount: 12,
    pendingShares: 1,
  },
  {
    orgId: "org-2",
    name: "Globex Design",
    role: "member",
    memberCount: 8,
    consented: true,
    lastSeq: 100,
    itemCount: 0,
    receivedCount: 57,
    pendingShares: 0,
  },
];

test.describe("Shared Brain v1 — org FE surfaces (mocked IPC)", () => {
  test("Settings › Organization renders the managed (multi-org) state", async ({
    page,
  }) => {
    await mockTauri(page, {
      account_status: ACCOUNT_STATUS,
      org_refresh: () => null,
      org_list_statuses: ORG_STATUSES,
      org_status: () => null,
      org_list_members: () => [
        {
          userId: "u-1",
          email: "you@example.com",
          role: "owner",
          addedAt: new Date().toISOString(),
          removed: false,
        },
        {
          userId: "u-2",
          email: "kasia@example.com",
          role: "member",
          addedAt: new Date().toISOString(),
          removed: false,
        },
      ],
    });
    await page.goto("/settings");
    await page.getByRole("button", { name: "Organization" }).first().click();

    await expect(page.locator("app-settings-organization-section")).toBeVisible(
      { timeout: 10_000 },
    );
    // Both org cards render, by name.
    await expect(page.getByText("Acme Inc.").first()).toBeVisible();
    await expect(page.getByText("Globex Design").first()).toBeVisible();
    // Sync + Invite affordances (the owned org exposes Invite).
    await expect(
      page.getByRole("button", { name: "Sync now" }).first(),
    ).toBeVisible();
    const owned = page.locator(".org-card", { hasText: "Acme Inc." });
    await expect(
      owned.getByRole("button", { name: "Invite & members" }),
    ).toBeVisible();
    // Expand the member manager to reveal the member list.
    await owned.getByRole("button", { name: "Invite & members" }).click();
    await expect(page.getByText("kasia@example.com")).toBeVisible();
    await expect(page.getByRole("button", { name: "Invite" })).toBeVisible();

    await page.screenshot({
      path: "e2e/org/__screens__/settings-organization.png",
      fullPage: true,
    });
  });

  test("Settings › Organization renders the empty state with no orgs", async ({
    page,
  }) => {
    await mockTauri(page, {
      account_status: ACCOUNT_STATUS,
      org_refresh: () => null,
      org_list_statuses: () => [],
      org_status: () => null,
    });
    await page.goto("/settings");
    await page.getByRole("button", { name: "Organization" }).first().click();

    await expect(page.getByText("Create an organization")).toBeVisible({
      timeout: 10_000,
    });
    await expect(
      page.getByRole("button", { name: "Create organization" }),
    ).toBeVisible();

    await page.screenshot({
      path: "e2e/org/__screens__/settings-organization-create.png",
      fullPage: true,
    });
  });

  test("Share panel shows the Org Brain flow + preview sheet", async ({
    page,
  }) => {
    await mockTauri(page, {
      account_status: ACCOUNT_STATUS,
      // The share panel's gate reads the single-org `org_status`; the sheet's
      // picker reads the multi-org `org_list_statuses` (fix C).
      org_status: () => ({
        orgId: "org-1",
        name: "Acme Inc.",
        role: "owner",
        memberCount: 3,
        consented: true,
        lastSeq: 42,
        itemCount: 12,
        receivedCount: 12,
        pendingShares: 1,
      }),
      org_list_statuses: () => [
        {
          orgId: "org-1",
          name: "Acme Inc.",
          role: "owner",
          memberCount: 3,
          consented: true,
          lastSeq: 42,
          itemCount: 12,
          receivedCount: 12,
          pendingShares: 1,
        },
      ],
      list_my_shares: () => [],
      list_org_shares: () => [],
      preview_org_share: () => ({
        title: "Weekly sync",
        markdown:
          "# Weekly sync\n\n- Shipped the org brain\n- Next: mobile\n\nContact: alex@example.com",
        bytes: 96,
        chunkCount: 2,
        scrubbed: { emails: 1, phones: 0, cards: 0 },
        scrub: true,
        attachmentCount: 0,
        attachmentBytes: 0,
        imagePixelsScrubbed: false,
      }),
    });
    // Open a meeting, go to the Share tab.
    await page.goto("/library");
    await page.locator("li.row-item a.row").first().click();
    await page
      .getByRole("tab", { name: /share/i })
      .first()
      .click()
      .catch(async () => {
        // Fallback if the tab is a button, not a role=tab.
        await page.getByText("Share", { exact: false }).first().click();
      });

    // The Org Brain section CTA.
    const addBtn = page
      .getByRole("button", { name: "Add to Org Brain" })
      .first();
    await expect(addBtn).toBeVisible({ timeout: 10_000 });
    await page.screenshot({
      path: "e2e/org/__screens__/share-panel-org-section.png",
      fullPage: true,
    });

    // Open the preview sheet. The host element has no size (its content is
    // position:fixed), so assert on the inner dialog + its content.
    await addBtn.click();
    await expect(
      page.locator("app-org-share-sheet [role='dialog']"),
    ).toBeVisible();
    // The exact outgoing markdown + the picker's single-org audience line render
    // (one org ⇒ a label, not a redundant picker — fix C).
    await expect(page.getByText("Exactly what leaves your Mac")).toBeVisible();
    await expect(
      page.locator("app-org-share-sheet").getByText("3 members of Acme Inc."),
    ).toBeVisible();
    await page.screenshot({
      path: "e2e/org/__screens__/org-share-sheet.png",
      fullPage: true,
    });
  });

  test("Lock×shares dialog blocks locking a shared folder", async ({
    page,
  }) => {
    await mockTauri(page, {
      // The ONE tree needs a folder to offer a lock on; the demo default has no hierarchy.
      list_workspace_tree: () => [
        {
          id: "p-root",
          name: "Workspace",
          level: "project",
          emoji: null,
          tint: null,
          locked: false,
          unlocked: false,
          isRoot: false,
          folders: [
            {
              id: "f-shared",
              name: "Shared work",
              level: "folder",
              emoji: null,
              tint: null,
              locked: false,
              unlocked: false,
              isRoot: false,
              folders: [],
              groups: [],
            },
          ],
          groups: [],
        },
      ],
      folder_active_shares: () => ({
        links: 1,
        users: 0,
        org: [{ itemId: "it-1", title: "Weekly sync" }],
      }),
    });
    await page.goto("/library");

    await expect(page.locator("app-library .library")).toBeVisible();
    await expect(page).toHaveURL(/\/library$/);
    await expect(page.locator("app-library .library")).toBeVisible();

    // The contextual tree exposes the lock menu without replacing the mounted
    // meetings surface. The shares gate remains unchanged: probe first, then
    // put the blocking dialog in front of the seal.
    // The Workspaces tree is a section of the ONE sidebar now, rather than a
    // separate "Workspaces sidebar" panel opened from a rail button.
    const spacesSidebar = page.getByRole("navigation", {
      name: "Primary navigation",
    });
    await expect(spacesSidebar).toBeVisible();
    const project = spacesSidebar.getByRole("treeitem").first();
    await expect(project).toBeVisible({ timeout: 10_000 });
    await spacesSidebar
      .getByRole("button", { name: /^Expand / })
      .first()
      .click();
    // Focus, not a pointer — see the note in lock-shares-dialog.spec.ts.
    // Expansion updates the flattened tree asynchronously. Target the intended
    // folder by its contextual label instead of racing `.last()` against that
    // update (which can leave focus on a detached project trigger).
    const sharedFolderActions = spacesSidebar.getByRole("button", {
      name: "Actions for Shared work",
    });
    await expect(sharedFolderActions).toBeVisible();
    await sharedFolderActions.focus();
    await page.keyboard.press("Enter");
    // The lock entry names the CASCADE now: locking a container seals every container inside it,
    // so a project holding folders says so rather than calling itself "Lock folder". This test is
    // about the shares gate, not the wording, so it matches the affordance rather than one label —
    // and the project is the case where the gate matters most, since the seal reaches its
    // descendants too.
    const lockFolder = page.getByRole("menuitem", { name: /^Lock folder/ });
    await expect(lockFolder).toBeVisible();
    await lockFolder.focus();
    await page.keyboard.press("Enter");

    // The blocking dialog appears with the three choices. The host has no size
    // (position:fixed content), so assert on the inner dialog.
    await expect(
      page.locator("app-lock-shares-dialog [role='dialog']"),
    ).toBeVisible({ timeout: 10_000 });
    await expect(
      page.getByRole("button", { name: "Revoke & lock" }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Lock anyway" }),
    ).toBeVisible();
    await page.screenshot({
      path: "e2e/org/__screens__/lock-shares-dialog.png",
      fullPage: true,
    });
  });
});
