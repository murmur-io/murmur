import { test, expect, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * ORG REPLICA CONVERGENCE (2026-07-26), item 5 — the org-item viewer must not keep
 * showing content the org has withdrawn.
 *
 * RED before this fix: `OrgItemViewerComponent` fetched `org_get_item` exactly once,
 * on route entry, and `TabRouteReuseStrategy` keeps the instance ALIVE while the tab
 * is backgrounded — so an item tombstoned by the backend (a colleague's revoke, a
 * feed tombstone, or the new anti-entropy reconcile sweep) stayed fully readable in
 * this view indefinitely, with no lifecycle hook that could ever tell it otherwise.
 *
 * GREEN: the viewer subscribes to `murmur://org-feed-updated` (content-free), re-fetches,
 * and — when the backend now answers `null` — drops the content, states plainly that the
 * note is no longer available, and closes the tab.
 *
 * Overrides run PAGE-SIDE (serialized to strings), so they must be self-contained.
 * `window.__orgItemWithdrawn` is the page-side switch the test flips to make the
 * backend "withdraw" the item mid-session.
 */

const ORG_STATUSES = () => [
  {
    orgId: "org-1",
    name: "Acme Inc.",
    role: "member",
    memberCount: 3,
    consented: true,
    lastSeq: 42,
    itemCount: 1,
    receivedCount: 1,
    pendingShares: 0,
    contextEnabled: true,
  },
];

const ORG_ITEMS = () => [
  {
    itemId: "it-1",
    title: "Acme onboarding brief",
    authorHint: "kasia",
    createdAt: "2026-07-10T09:00:00Z",
    seq: 2,
  },
];

/**
 * `org_get_item` that honors the page-side withdrawal switch: the real backend
 * returns `None` for a tombstoned (or context-disabled) item, and that is exactly
 * the signal the viewer converges on.
 */
const ORG_GET_ITEM = () =>
  (window as unknown as { __orgItemWithdrawn?: boolean }).__orgItemWithdrawn
    ? null
    : {
        itemId: "it-1",
        authorHint: "kasia",
        title: "Acme onboarding brief",
        createdAt: "2026-07-10T09:00:00Z",
        rev: 3,
        markdown:
          "# Acme onboarding brief\n\n- Kickoff Monday\n- Owner: Kasia\n- zephyrine budget approved",
        editable: false,
      };

/** Fire the content-free backend ping the background sync / reconcile sweep emits. */
async function emitOrgFeedUpdated(page: Page) {
  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://org-feed-updated", { orgsChanged: 1 });
  });
}

test.describe("org-item viewer — withdrawn content (mocked IPC)", () => {
  test("an org-feed update that withdraws the item evicts the open viewer", async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    page.on("console", (m) => {
      if (m.type() === "error") {
        consoleErrors.push(m.text());
      }
    });
    page.on("pageerror", (e) => consoleErrors.push(String(e)));

    await mockTauri(page, {
      org_resolve_source: () => null,
      org_get_item: ORG_GET_ITEM,
      org_refresh: () => null,
      org_list_statuses: ORG_STATUSES,
      list_org_items: ORG_ITEMS,
      list_note_attachments: () => [],
    });
    await page.goto("/org-item/it-1");

    // The read-only view renders the decrypted body.
    await expect(page.locator(".oi-title")).toHaveText("Acme onboarding brief", {
      timeout: 10_000,
    });
    await expect(
      page.getByText("zephyrine budget approved").first(),
    ).toBeVisible();

    // A ping that does NOT withdraw anything must leave the view exactly as it was
    // (a re-fetch is not an excuse to flash or blank a healthy item).
    await emitOrgFeedUpdated(page);
    await expect(page.locator(".oi-title")).toHaveText("Acme onboarding brief");

    // The org withdraws the item; the next feed ping is the only signal this
    // already-rendered, tab-cached view will ever get.
    await page.evaluate(() => {
      (window as unknown as { __orgItemWithdrawn?: boolean }).__orgItemWithdrawn =
        true;
    });
    await emitOrgFeedUpdated(page);

    // The withdrawn content is GONE from the view — not merely stale.
    const removed = page.locator("[data-testid='org-item-removed']");
    await expect(removed).toBeVisible({ timeout: 10_000 });
    await expect(page.locator(".oi-title")).toHaveCount(0);
    await expect(page.getByText("zephyrine budget approved")).toHaveCount(0);
    await expect(removed).toContainText("This shared note is no longer available");

    expect(consoleErrors, `console errors: ${consoleErrors.join(" | ")}`).toEqual(
      [],
    );
  });

  test("a stale link to an already-withdrawn item says so instead of rendering an empty shell", async ({
    page,
  }) => {
    await mockTauri(page, {
      org_resolve_source: () => null,
      // Already withdrawn before this view ever opened (a stale citation/bookmark).
      org_get_item: () => null,
      org_refresh: () => null,
      org_list_statuses: ORG_STATUSES,
      list_org_items: ORG_ITEMS,
      list_note_attachments: () => [],
    });
    await page.goto("/org-item/it-1");

    await expect(page.locator("[data-testid='org-item-removed']")).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator(".oi-title")).toHaveCount(0);

    // The dead-end has a way out.
    await page.getByRole("button", { name: "Back to Notes" }).click();
    await expect(page).toHaveURL(/\/notes$/, { timeout: 10_000 });
  });
});
