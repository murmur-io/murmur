import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * `/org-item/:id` viewer — REMOVE-FROM-ORG affordance (Bug B, root-cause fix
 * 2026-07-15): an author viewing their OWN shared item from a device that
 * never itself shared it (`org_resolve_source` → `null`, same as any other
 * non-origin device — see `org-item-viewer.component.ts`'s `resolveThenLoad`)
 * had NO way to remove it at all — `org-item-viewer.component.html` offered
 * only Edit/Save/Cancel. This confirms the new "Remove" affordance renders
 * for an `editable: true` item, its confirm step calls the new
 * `delete_org_item_as_author` command, and a successful removal navigates
 * back to Notes.
 */
test.describe("org-item viewer — the author can remove their own item from a non-origin device", () => {
  test("Remove asks to confirm, then calls delete_org_item_as_author and navigates back", async ({
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
          contextEnabled: true,
        },
      ],
      list_org_items: () => [
        {
          itemId: "oi1",
          title: "My Roadmap",
          authorHint: "me",
          createdAt: "2026-07-09T10:00:00Z",
          seq: 5,
        },
      ],
      // editable:true — this device is NOT the origin (org_resolve_source
      // defaults to null in the base mock), yet the item is the caller's own.
      org_get_item: (args: { itemId: string }) => ({
        itemId: args.itemId,
        authorHint: "me",
        title: "My Roadmap",
        createdAt: "2026-07-09T10:00:00Z",
        rev: 1,
        markdown: "# My Roadmap\n\nShip it.",
        editable: true,
      }),
      // Overrides run page-side (mockTauri serializes them to a string) — no
      // closures over test-scope, so track calls on `window` like the other
      // specs in this suite (e.g. note-autosave-cross-tab-loss.spec.ts).
      delete_org_item_as_author: (args: { itemId: string }) => {
        const w = window as unknown as { __deleteCalls?: string[] };
        w.__deleteCalls ??= [];
        w.__deleteCalls.push(args.itemId);
        return null;
      },
    });

    await page.goto("/org-item/oi1");
    await expect(page.locator(".oi-title")).toHaveText("My Roadmap");

    // Edit + Remove both present for an editable item; no confirm yet.
    const removeBtn = page.locator(".oi-remove-btn");
    await expect(removeBtn).toBeVisible();
    await expect(page.locator("text=Remove from the org?")).toHaveCount(0);

    await removeBtn.click();
    await expect(page.locator("text=Remove from the org?")).toBeVisible();
    // No IPC call yet — the confirm step must not have fired the delete itself.
    let deleteCalls = await page.evaluate(
      () => (window as unknown as { __deleteCalls?: string[] }).__deleteCalls ?? [],
    );
    expect(deleteCalls).toEqual([]);

    await page.locator(".btn-danger", { hasText: "Remove" }).click();

    await expect(async () => {
      deleteCalls = await page.evaluate(
        () => (window as unknown as { __deleteCalls?: string[] }).__deleteCalls ?? [],
      );
      expect(deleteCalls).toEqual(["oi1"]);
    }).toPass({ timeout: 2_000 });

    // Successful removal navigates back to Notes.
    await expect(page).toHaveURL(/\/notes$/);

    expect(consoleErrors).toEqual([]);
  });

  test("Cancel dismisses the confirm without calling delete_org_item_as_author", async ({
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
      org_get_item: (args: { itemId: string }) => ({
        itemId: args.itemId,
        authorHint: "me",
        title: "My Roadmap",
        createdAt: "2026-07-09T10:00:00Z",
        rev: 1,
        markdown: "# My Roadmap\n\nShip it.",
        editable: true,
      }),
      delete_org_item_as_author: (args: { itemId: string }) => {
        const w = window as unknown as { __deleteCalls?: string[] };
        w.__deleteCalls ??= [];
        w.__deleteCalls.push(args.itemId);
        return null;
      },
    });

    await page.goto("/org-item/oi1");
    await expect(page.locator(".oi-title")).toHaveText("My Roadmap");

    await page.locator(".oi-remove-btn").click();
    await expect(page.locator("text=Remove from the org?")).toBeVisible();
    await page.locator("button", { hasText: "Cancel" }).click();

    await expect(page.locator("text=Remove from the org?")).toHaveCount(0);
    await expect(page.locator(".oi-remove-btn")).toBeVisible();
    const deleteCalls = await page.evaluate(
      () => (window as unknown as { __deleteCalls?: string[] }).__deleteCalls ?? [],
    );
    expect(deleteCalls).toEqual([]);
    // Still on the viewer — Cancel did not navigate away.
    await expect(page).toHaveURL(/\/org-item\/oi1$/);

    expect(consoleErrors).toEqual([]);
  });
});
