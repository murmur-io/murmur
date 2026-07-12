import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Notes home — org (Shared Brain) surfacing. Confirms that with a mocked org
 * (`org_list_statuses`) carrying a shared item (`list_org_items`), the Notes view:
 *   1. renders the content pane's "Shared brains" CHIP ROW listing the org
 *      (2026-07-12 — moved out of the rail when the rail itself moved into the
 *      main sidebar's note-FOLDER tree; orgs aren't note-folders, so they stay
 *      a content-pane affordance, not part of the sidebar tree);
 *   2. MERGES the org's shared item into the "All notes" TABLE (2026-07-12,
 *      replaces the card grid) alongside YOUR authored notes, with the
 *      ORG-NAME badge on the org row;
 *   3. routes to the READ-ONLY `/org-item/:id` viewer when the org row is clicked
 *      (your own notes still open the `/notes/:id` editor).
 * All under the mocked IPC with NO console/page errors — the runtime check that
 * catches NG0600 / ɵcmp / forwardRef / routerLink regressions a green build misses.
 *
 * The org command NAMES + camelCase DTOs match `ipc.service.ts` / `models.ts`:
 *   org_refresh → void · org_list_statuses → OrgStatus[] · list_org_items →
 *   OrgItemHeader[] · org_get_item → OrgItemDetail.
 */

/** The mocked org command overrides shared by every case in this file. */
const ORG_MOCKS = {
  org_refresh: () => null,
  org_list_statuses: () => [
    {
      orgId: "org1",
      name: "Siema",
      role: "member",
      memberCount: 3,
      consented: true,
      lastSeq: 5,
      itemCount: 0,
      receivedCount: 1,
      pendingShares: 0,
    },
  ],
  list_org_items: (args: { orgId: string }) =>
    args.orgId === "org1"
      ? [
          {
            itemId: "oi1",
            title: "Team Roadmap Q3",
            authorHint: "alice",
            createdAt: "2026-07-09T10:00:00Z",
            seq: 5,
          },
        ]
      : [],
  org_get_item: (args: { itemId: string }) => ({
    itemId: args.itemId,
    authorHint: "alice",
    title: "Team Roadmap Q3",
    createdAt: "2026-07-09T10:00:00Z",
    rev: 1,
    markdown: "# Team Roadmap Q3\n\nShip the org brain.",
  }),
};

test("All notes merges org shared items (with org-name badge) + the chip row lists the org", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, ORG_MOCKS);
  await page.goto("/notes");

  await expect(page.locator(".notes-content")).toBeVisible();

  // 1) The content pane's "Shared brains" chip row lists the org.
  const orgChip = page.locator(".org-chip", { hasText: "Siema" });
  await expect(orgChip).toHaveCount(1);

  // 2) "All notes" shows YOUR authored note AND the org shared item, merged,
  // as table rows.
  await expect(page.getByText("My First Note")).toBeVisible();
  const orgRow = page.locator(".mur-table tbody tr", { hasText: "Team Roadmap Q3" });
  await expect(orgRow).toHaveCount(1);
  // The org row carries the ORG-NAME badge + the author hint.
  await expect(orgRow.locator(".org-badge")).toContainText("Siema");
  await expect(orgRow.locator(".note-author")).toHaveText("alice");

  expect(consoleErrors).toEqual([]);
});

test("a kind:'meeting' org item is excluded from Notes (it belongs in Library's Shared brains chip row)", async ({
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
    ...ORG_MOCKS,
    list_org_items: (args: { orgId: string }) =>
      args.orgId === "org1"
        ? [
            {
              itemId: "oi-doc",
              title: "Team Roadmap Q3",
              authorHint: "alice",
              createdAt: "2026-07-09T10:00:00Z",
              seq: 5,
              kind: "document",
            },
            {
              itemId: "oi-meeting",
              title: "Weekly Sync Notes",
              authorHint: "bob",
              createdAt: "2026-07-10T10:00:00Z",
              seq: 6,
              kind: "meeting",
            },
          ]
        : [],
  });
  await page.goto("/notes");

  await expect(page.locator(".notes-content")).toBeVisible();

  // "All notes": the document-kind item shows, the meeting-kind one does NOT.
  await expect(page.getByText("Team Roadmap Q3")).toBeVisible();
  await expect(page.getByText("Weekly Sync Notes")).toHaveCount(0);

  // Selecting the org chip: same exclusion applies to the org-scoped view.
  await page.locator(".org-chip", { hasText: "Siema" }).click();
  await expect(page.getByText("Team Roadmap Q3")).toBeVisible();
  await expect(page.getByText("Weekly Sync Notes")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});

test("clicking an org shared item routes to the read-only /org-item viewer", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, ORG_MOCKS);
  await page.goto("/notes");
  await expect(page.locator(".notes-content")).toBeVisible();

  // Click the org row's title link → the read-only org-item viewer route.
  await page
    .locator(".mur-table tbody tr", { hasText: "Team Roadmap Q3" })
    .locator(".title-link")
    .click();
  await expect(page).toHaveURL(/\/org-item\/oi1$/);
  // The viewer rendered the decrypted org item body (read-only, no editor).
  await expect(page.locator("app-org-item-viewer")).toBeVisible();
  await expect(page.getByText("Ship the org brain.")).toBeVisible();

  expect(consoleErrors).toEqual([]);
});

test("selecting the org chip shows ONLY that org's items", async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, ORG_MOCKS);
  await page.goto("/notes");
  await expect(page.locator(".notes-content")).toBeVisible();

  // Select the org chip → the pane scopes to its items only.
  await page.locator(".org-chip", { hasText: "Siema" }).click();
  await expect(page.locator(".content-title")).toHaveText("Siema");
  await expect(
    page.locator(".mur-table tbody tr", { hasText: "Team Roadmap Q3" }),
  ).toHaveCount(1);
  // Your authored notes are NOT in the org-scoped view.
  await expect(page.getByText("My First Note")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});
