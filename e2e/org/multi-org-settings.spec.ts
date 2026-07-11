import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Multi-org Settings › Organizations (fix/org-multi-membership) — the section
 * now lists EVERY org the user belongs to (created OR invited-into), not just
 * the first locally-created one. Driven with a mocked Tauri IPC (no Rust core):
 * `org_refresh` (server membership discovery, a no-op here) then
 * `org_list_statuses` feed the list.
 *
 * Asserts: TWO orgs render, each with its name + the right OWNER/MEMBER role
 * badge (so it's obvious WHICH orgs the user is in and their role), and the
 * empty state when the list is empty. Render + no-console-error smoke; NOT a
 * proof of end-to-end behavior (no OCK / no server).
 *
 * Overrides run PAGE-SIDE (serialized to strings), so they must be
 * self-contained — no closures over test scope.
 */

// A signed-in, sharing-ready account so the header can show "Signed in as …".
const ACCOUNT_STATUS = () => ({
  loggedIn: true,
  email: "you@example.com",
  unlockedForSharing: true,
  shareConsented: true,
  serverConfigured: true,
  biometricUnlockAvailable: true,
});

// Two orgs: one this user OWNS, one they're only a MEMBER of (invited-into).
const TWO_ORGS = () => [
  {
    orgId: "org-owned",
    name: "Acme Inc.",
    role: "owner",
    memberCount: 3,
    consented: true,
    lastSeq: 42,
    itemCount: 12,
    receivedCount: 12,
    pendingShares: 0,
  },
  {
    orgId: "org-joined",
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

async function openOrgSection(page: import("@playwright/test").Page): Promise<void> {
  await page.goto("/settings");
  await page.getByRole("button", { name: "Organization" }).first().click();
  await expect(
    page.locator("app-settings-organization-section"),
  ).toBeVisible({ timeout: 10_000 });
}

test.describe("Settings › Organizations — multi-org (mocked IPC)", () => {
  test("renders TWO orgs with the right role badges", async ({ page }) => {
    const errors: string[] = [];
    page.on("console", (m) => {
      if (m.type() === "error") {
        errors.push(m.text());
      }
    });

    await mockTauri(page, {
      account_status: ACCOUNT_STATUS,
      org_refresh: () => null,
      org_list_statuses: TWO_ORGS,
      // legacy single-org path must NOT be what drives the list anymore
      org_status: () => null,
    });
    await openOrgSection(page);

    // Header context.
    await expect(page.getByText("Signed in as")).toBeVisible();
    await expect(page.getByText("you@example.com")).toBeVisible();

    // Both org cards render, by name.
    const owned = page.locator(".org-card", { hasText: "Acme Inc." });
    const joined = page.locator(".org-card", { hasText: "Globex Design" });
    await expect(owned).toBeVisible();
    await expect(joined).toBeVisible();

    // Distinct role badges, scoped to their own card.
    await expect(owned.locator(".org-role-badge.is-owner")).toHaveText("Owner");
    await expect(joined.locator(".org-role-badge.is-member")).toHaveText(
      "Member",
    );

    // The OWNED org exposes Invite; the MEMBER-only org does NOT.
    await expect(
      owned.getByRole("button", { name: "Invite & members" }),
    ).toBeVisible();
    await expect(
      joined.getByRole("button", { name: "Invite & members" }),
    ).toHaveCount(0);

    // Counts render per card.
    await expect(owned.getByText("3 members")).toBeVisible();
    await expect(joined.getByText("8 members")).toBeVisible();

    await page.screenshot({
      path: "e2e/org/__screens__/settings-multi-org.png",
      fullPage: true,
    });

    expect(errors, `console errors: ${errors.join("\n")}`).toEqual([]);
  });

  test("shows the empty state when the user is in no org", async ({ page }) => {
    await mockTauri(page, {
      account_status: ACCOUNT_STATUS,
      org_refresh: () => null,
      org_list_statuses: () => [],
      org_status: () => null,
    });
    await openOrgSection(page);

    await expect(
      page.getByText(
        "You're not in any organization yet — create one, or ask a teammate to invite you by email.",
      ),
    ).toBeVisible();
    // The create form is still available.
    await expect(
      page.getByRole("button", { name: "Create organization" }),
    ).toBeVisible();
    // No org cards.
    await expect(page.locator(".org-card")).toHaveCount(0);

    await page.screenshot({
      path: "e2e/org/__screens__/settings-multi-org-empty.png",
      fullPage: true,
    });
  });
});
