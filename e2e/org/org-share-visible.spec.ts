import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * feat/org-share-visible — the remediation for "a member can't see a note a
 * colleague shared into their org". Two surfaces, driven against the extended
 * spec DTO contract with a mocked Tauri IPC (no Rust core):
 *
 *  FIX A — Settings › Organization now BROWSES an org's shared brain: expanding
 *          "Shared brain" lists `list_org_items(orgId)` (title + author + date),
 *          each row a link to the /org-item/:id viewer. So a member finally has
 *          somewhere to SEE what was shared in.
 *
 *  FIX C — the org-share sheet is a PICKER: it loads `org_list_statuses`, shows a
 *          select of the user's orgs, and threads the CHOSEN orgId through
 *          `share_document_to_org` (previously it shared to the FIRST org). We
 *          record the invoke args page-side and assert the chosen orgId reaches
 *          the command.
 *
 * Overrides run PAGE-SIDE (serialized to strings — self-contained, no closures).
 * Render + arg-threading smoke; NOT proof of end-to-end behavior (no OCK / no
 * server).
 */

const ACCOUNT_STATUS = () => ({
  loggedIn: true,
  email: "you@example.com",
  unlockedForSharing: true,
  shareConsented: true,
  serverConfigured: true,
  biometricUnlockAvailable: true,
});

// Two orgs the user belongs to (drives both the Settings list AND the picker).
const TWO_ORGS = () => [
  {
    orgId: "org-owned",
    name: "Acme Inc.",
    role: "owner",
    memberCount: 3,
    consented: true,
    lastSeq: 42,
    itemCount: 2,
    receivedCount: 5,
    pendingShares: 0,
  },
  {
    orgId: "org-siema",
    name: "Siema",
    role: "member",
    memberCount: 4,
    consented: true,
    lastSeq: 100,
    itemCount: 0,
    receivedCount: 3,
    pendingShares: 0,
  },
];

test.describe("Org share — browse + picker (mocked IPC)", () => {
  test("FIX A — Settings › Organization browses an org's shared items", async ({
    page,
  }) => {
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
      org_status: () => null,
      // One item shared INTO the "Siema" org — what a colleague published.
      list_org_items: (args: { orgId: string }) =>
        args.orgId === "org-siema"
          ? [
              {
                itemId: "it-42",
                title: "Q3 planning recap",
                authorHint: "kasia",
                createdAt: new Date("2026-07-10T09:00:00Z").toISOString(),
                seq: 100,
              },
            ]
          : [],
    });

    await page.goto("/settings");
    await page.getByRole("button", { name: "Organization" }).first().click();
    await expect(
      page.locator("app-settings-organization-section"),
    ).toBeVisible({ timeout: 10_000 });

    // FIX B — the counts are labelled separately (kills the "0 items" lie): the
    // member-only "Siema" org received items even though it uploaded none.
    const siema = page.locator(".org-card", { hasText: "Siema" });
    await expect(siema.getByText("3 in the org brain")).toBeVisible();
    await expect(siema.getByText("0 shared by you")).toBeVisible();

    // Expand the shared-brain browse list and see the colleague's item.
    await siema.getByRole("button", { name: "Shared brain" }).click();
    const itemLink = siema.locator(".org-item-link", {
      hasText: "Q3 planning recap",
    });
    await expect(itemLink).toBeVisible();
    await expect(itemLink.getByText("kasia", { exact: false })).toBeVisible();
    // The row links to the read-only org-item viewer.
    await expect(itemLink).toHaveAttribute("href", /\/org-item\/it-42/);

    await page.screenshot({
      path: "e2e/org/__screens__/settings-org-browse.png",
      fullPage: true,
    });

    expect(errors, `console errors: ${errors.join("\n")}`).toEqual([]);
  });

  test("FIX A — empty state when nothing is shared into the org", async ({
    page,
  }) => {
    await mockTauri(page, {
      account_status: ACCOUNT_STATUS,
      org_refresh: () => null,
      org_list_statuses: TWO_ORGS,
      org_status: () => null,
      list_org_items: () => [],
    });
    await page.goto("/settings");
    await page.getByRole("button", { name: "Organization" }).first().click();
    const acme = page.locator(".org-card", { hasText: "Acme Inc." });
    await acme.getByRole("button", { name: "Shared brain" }).click();
    await expect(
      acme.getByText("Nothing shared into this org yet."),
    ).toBeVisible();
  });

  test("FIX C — the share sheet picks an org and threads the chosen orgId", async ({
    page,
  }) => {
    // Record share_meeting_to_org invocations page-side so we can assert the
    // CHOSEN orgId reaches the command.
    await page.addInitScript(() => {
      (window as unknown as { __shareCalls: unknown[] }).__shareCalls = [];
    });

    await mockTauri(page, {
      account_status: ACCOUNT_STATUS,
      // The picker loads the multi-org list.
      org_list_statuses: TWO_ORGS,
      // The share panel's gate section still reads the single-org status.
      org_status: () => ({
        orgId: "org-owned",
        name: "Acme Inc.",
        role: "owner",
        memberCount: 3,
        consented: true,
        lastSeq: 42,
        itemCount: 2,
        receivedCount: 5,
        pendingShares: 0,
      }),
      list_my_shares: () => [],
      list_org_shares: () => [],
      preview_org_share: () => ({
        title: "Weekly sync",
        markdown: "# Weekly sync\n\n- Shipped the org brain",
        bytes: 64,
        chunkCount: 1,
        scrubbed: { emails: 0, phones: 0, cards: 0 },
        scrub: true,
      }),
      share_meeting_to_org: (args: unknown) => {
        (window as unknown as { __shareCalls: unknown[] }).__shareCalls.push(
          args,
        );
        return null;
      },
    });

    // Open a meeting, go to the Share tab → the Org Brain sheet.
    await page.goto("/library");
    await page.locator("li.row-item a.row").first().click();
    await page
      .getByRole("tab", { name: /share/i })
      .first()
      .click()
      .catch(async () => {
        await page.getByText("Share", { exact: false }).first().click();
      });

    const addBtn = page
      .getByRole("button", { name: "Add to Org Brain" })
      .first();
    await expect(addBtn).toBeVisible({ timeout: 10_000 });
    await addBtn.click();

    const dialog = page.locator("app-org-share-sheet [role='dialog']");
    await expect(dialog).toBeVisible();

    // The PICKER is present with BOTH orgs (2 options), defaulting to the first.
    const select = dialog.locator("mur-select select");
    await expect(select).toBeVisible();
    await expect(select.locator("option")).toHaveCount(2);
    await expect(select.locator("option").nth(0)).toHaveText("Acme Inc.");
    await expect(select.locator("option").nth(1)).toHaveText("Siema");

    // Choose "Siema" (org-siema) — the org the colleague couldn't see into.
    await select.selectOption("org-siema");
    // The audience line reflects the chosen org.
    await expect(dialog.getByText("4 members of Siema")).toBeVisible();

    // Confirm the publish; the CHOSEN orgId must reach share_document_to_org.
    await dialog.getByRole("button", { name: "Add to Org Brain" }).click();

    await expect
      .poll(async () =>
        page.evaluate(
          () =>
            (window as unknown as { __shareCalls: { orgId?: string }[] })
              .__shareCalls,
        ),
      )
      .toContainEqual(expect.objectContaining({ orgId: "org-siema" }));

    await page.screenshot({
      path: "e2e/org/__screens__/org-share-picker.png",
      fullPage: true,
    });
  });

  test("invite-only member (not first in org_status) still sees the Add to Org Brain CTA", async ({
    page,
  }) => {
    // Regression for the legacy-single-org bug: `SharePanelComponent` used to gate
    // its "Add to Org Brain" section on `org_status` — which the backend documents
    // as `list_org_states().into_iter().next()`, i.e. only the FIRST locally-joined
    // org (kept for legacy callers). A user whose only membership came from an
    // invite (never created an org locally) can have `org_status` resolve to null
    // even though `org_list_statuses` correctly lists their org. Every other
    // multi-org surface (Settings, the org-share sheet, the org-item viewer) reads
    // `org_list_statuses`; the Share panels must too.
    await mockTauri(page, {
      account_status: ACCOUNT_STATUS,
      // Simulates the invite-only member: the legacy single-org lookup finds
      // nothing (the joined org isn't first — or the local `next()` genuinely
      // returns null for this membership shape), but the full list has it.
      org_status: () => null,
      org_list_statuses: () => [
        {
          orgId: "org-invited",
          name: "Invited Co.",
          role: "member",
          memberCount: 6,
          consented: true,
          lastSeq: 12,
          itemCount: 0,
          receivedCount: 2,
          pendingShares: 0,
        },
      ],
      list_my_shares: () => [],
      list_org_shares: () => [],
      org_live_shares_for_source: () => [],
    });

    await page.goto("/library");
    await page.locator("li.row-item a.row").first().click();
    await page
      .getByRole("tab", { name: /share/i })
      .first()
      .click()
      .catch(async () => {
        await page.getByText("Share", { exact: false }).first().click();
      });

    // The CTA must render even though `org_status` resolved to null — it should
    // be driven by `org_list_statuses` like every other multi-org surface.
    await expect(
      page.getByRole("button", { name: "Add to Org Brain" }).first(),
    ).toBeVisible({ timeout: 10_000 });
  });
});
