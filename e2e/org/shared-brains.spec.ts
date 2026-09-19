import { mockDestinationPicker } from "../notes/destination-picker-mock";
import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const ORGS = [
  {
    orgId: "org-acme",
    name: "Acme",
    role: "member",
    memberCount: 3,
    consented: true,
    lastSeq: 5,
    itemCount: 1,
    receivedCount: 3,
    pendingShares: 0,
    contextEnabled: true,
  },
  {
    orgId: "org-studio",
    name: "Studio",
    role: "owner",
    memberCount: 2,
    consented: true,
    lastSeq: 2,
    itemCount: 1,
    receivedCount: 1,
    pendingShares: 0,
    contextEnabled: true,
  },
];

const FOREST = [
  {
    id: "space-product",
    name: "Product",
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
  {
    id: "space-unlocked-private",
    name: "Unlocked private",
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    locked: true,
    unlocked: true,
    isRoot: false,
    folders: [],
    groups: [],
  },
];

function watchRuntimeErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  return errors;
}

async function boot(
  page: Page,
  viewport?: { width: number; height: number },
): Promise<void> {
  if (viewport) {
    await page.setViewportSize(viewport);
  }
  await mockTauri(
    page,
    {
      list_org_items: (args: { orgId: string }) => {
        const rows = {
          "org-acme": [
            {
              itemId: "shared-meeting",
              title: "Customer review",
              authorHint: "ana",
              createdAt: "2026-08-25T10:00:00Z",
              seq: 5,
              kind: "meeting",
              ownedSource: null,
            },
            {
              itemId: "shared-note",
              title: "Research brief",
              authorHint: "sam",
              createdAt: "2026-08-24T10:00:00Z",
              seq: 4,
              kind: "document",
              ownedSource: null,
            },
            {
              itemId: "legacy-item",
              title: "Legacy shared item",
              authorHint: "lee",
              createdAt: "2026-08-23T10:00:00Z",
              seq: 3,
              kind: null,
              ownedSource: null,
            },
          ],
          "org-studio": [
            {
              itemId: "owned-meeting-share",
              title: "Studio planning",
              authorHint: "you",
              createdAt: "2026-08-22T10:00:00Z",
              seq: 2,
              kind: "meeting",
              ownedSource: { kind: "meeting", id: "meeting-local", movable: true },
            },
          ],
        } as Record<string, unknown[]>;
        return rows[args.orgId] ?? [];
      },
      add_org_item_to_container: (args: unknown) => {
        const target = window as unknown as { __orgImports?: unknown[] };
        (target.__orgImports ??= []).push(args);
        return { kind: "note", id: "note-imported" };
      },
      move_note: (args: unknown) => {
        const target = window as unknown as { __ownedMoves?: unknown[] };
        (target.__ownedMoves ??= []).push(args);
        return null;
      },
      org_refresh: () => {
        const target = window as unknown as { __orgRefreshCalls?: number };
        target.__orgRefreshCalls = (target.__orgRefreshCalls ?? 0) + 1;
        return null;
      },
      org_list_statuses: () => {
        const target = window as unknown as {
          __orgStatusRefreshCalls?: number;
        };
        target.__orgStatusRefreshCalls =
          (target.__orgStatusRefreshCalls ?? 0) + 1;
        return [];
      },
    },
    {
      org_list_cached_statuses: ORGS,
      list_workspace_tree: FOREST,
    },
  );
  await mockDestinationPicker(page);
  await page.goto("/shared-brains");
  await expect(
    page.getByRole("heading", { name: "Shared Brains" }),
  ).toBeVisible();
}

test("opening Shared Brains reads the local replica without a network refresh", async ({
  page,
}) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await boot(page);

  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(
    page.getByText("Customer review", { exact: true }),
  ).toBeVisible();

  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __orgRefreshCalls?: number })
            .__orgRefreshCalls ?? 0,
      ),
    )
    .toBe(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __orgStatusRefreshCalls?: number })
            .__orgStatusRefreshCalls ?? 0,
      ),
    )
    .toBe(0);
  expect(runtimeErrors).toEqual([]);
});

test("a local membership gate closing evicts previously rendered Shared Brain metadata", async ({
  page,
}) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await mockTauri(
    page,
    {
      org_list_cached_statuses: () => {
        const closed = (
          window as unknown as { __orgMembershipClosed?: boolean }
        ).__orgMembershipClosed;
        return closed
          ? []
          : [
              {
                orgId: "org-gated",
                name: "Gated organization",
                role: "member",
                memberCount: 2,
                consented: true,
                lastSeq: 1,
                itemCount: 0,
                receivedCount: 1,
                pendingShares: 0,
                contextEnabled: true,
              },
            ];
      },
      list_org_items: () => {
        const closed = (
          window as unknown as { __orgMembershipClosed?: boolean }
        ).__orgMembershipClosed;
        return closed
          ? []
          : [
              {
                itemId: "gated-item",
                title: "GATED_SECRET_TITLE",
                authorHint: "GATED_SECRET_AUTHOR",
                createdAt: "2026-08-26T09:00:00Z",
                seq: 1,
                kind: "document",
                ownedSource: null,
              },
            ];
      },
      org_refresh: () => {
        throw new Error("passive Shared Brains focus must not refresh");
      },
      org_list_statuses: () => {
        throw new Error("passive Shared Brains focus must not fetch status");
      },
    },
    { list_workspace_tree: FOREST },
  );
  await mockDestinationPicker(page);
  await page.goto("/shared-brains");
  await expect(
    page.getByText("GATED_SECRET_TITLE", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("GATED_SECRET_AUTHOR", { exact: true }),
  ).toBeVisible();

  await page.evaluate(() => {
    (
      window as unknown as { __orgMembershipClosed?: boolean }
    ).__orgMembershipClosed = true;
    window.dispatchEvent(new Event("focus"));
  });

  await expect(
    page.getByText("GATED_SECRET_TITLE", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("GATED_SECRET_AUTHOR", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Gated organization", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "No Shared Brains yet" }),
  ).toBeVisible();
  expect(runtimeErrors).toEqual([]);
});

test("Shared Brains keeps its org/type filters and explicit legacy rows beside the Workspaces tree", async ({
  page,
}) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await boot(page);

  // Shared Brains stopped being a rail destination on 2026-08-29: it is now a
  // ROW in the Workspaces sidebar (a virtual Workspace holding everything shared with
  // you that has no container of its own), so there is no rail link to be
  // current, and the tree stays beside the page instead of being replaced by
  // it. What the page itself does — the org and type filters, and the honest
  // "Unclassified" row for a legacy item with no source kind — is unchanged,
  // and that is what the rest of this test still pins.
  await expect(page.getByRole("link", { name: "Shared Brains" })).toHaveCount(0);
  await expect(
    page.getByText("Customer review", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("Research brief", { exact: true })).toBeVisible();
  await expect(
    page.getByText("Legacy shared item", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("Unclassified", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Notes", exact: true }).click();
  await expect(page.getByText("Research brief", { exact: true })).toBeVisible();
  await expect(page.getByText("Customer review", { exact: true })).toHaveCount(
    0,
  );
  await expect(
    page.getByText("Legacy shared item", { exact: true }),
  ).toHaveCount(0);

  await page.getByRole("button", { name: "All", exact: true }).click();
  await page.getByRole("button", { name: "Studio", exact: true }).click();
  await expect(
    page.getByText("Studio planning", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("Research brief", { exact: true })).toHaveCount(
    0,
  );
  expect(runtimeErrors).toEqual([]);
});

test("received replicas open in the read-only org viewer", async ({ page }) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await boot(page);

  await page.locator(".row-main", { hasText: "Customer review" }).click();
  await expect(page).toHaveURL(/\/org-item\/shared-meeting$/);
  expect(runtimeErrors).toEqual([]);
});

test("the received-item viewer resolves its organization locally", async ({
  page,
}) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await mockTauri(
    page,
    {
      org_resolve_source: () => null,
      org_get_item: () => ({
        itemId: "shared-note",
        authorHint: "ana",
        title: "Local viewer item",
        createdAt: "2026-08-25T10:00:00Z",
        rev: 1,
        markdown: "# Local viewer item\n\nShared body",
      }),
      list_org_items: () => [
        {
          itemId: "shared-note",
          title: "Local viewer item",
          authorHint: "ana",
          createdAt: "2026-08-25T10:00:00Z",
          seq: 1,
          kind: "document",
          ownedSource: null,
        },
      ],
      org_refresh: () => {
        throw new Error(
          "opening an admitted replica must not refresh membership",
        );
      },
      org_list_statuses: () => {
        throw new Error(
          "opening an admitted replica must not fetch live status",
        );
      },
      list_note_attachments: () => [],
      account_status: () => ({ loggedIn: false }),
    },
    {
      org_list_cached_statuses: [ORGS[0]],
      list_workspace_tree: FOREST,
    },
  );

  await mockDestinationPicker(page);
  await page.goto("/org-item/shared-note");
  await expect(page.locator(".oi-title")).toHaveText("Local viewer item");
  await expect(page.locator("app-note-document .origin-strip").getByText("Acme", { exact: true })).toBeVisible();
  expect(runtimeErrors).toEqual([]);
});

test("received replicas add a snapshot copy to a Workspace", async ({ page }) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await boot(page);

  await page
    .getByRole("button", { name: "Actions for Research brief" })
    .click();
  await page.getByRole("menuitem", { name: "Add a copy to Workspace…" }).click();
  const copySheet = page.getByRole("dialog", {
    name: "Add a copy “Research brief”",
  });
  await expect(
    copySheet.getByRole("button", { name: "Choose Unlocked private" }),
  ).toHaveCount(0);
  await copySheet
    .getByRole("button", { name: "Choose Product" })
    .click();
  await copySheet.getByRole("button", { name: "Add a copy here", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __orgImports?: unknown[] }).__orgImports ??
          [],
      ),
    )
    .toEqual([{ itemId: "shared-note", containerId: "space-product" }]);
  await expect(page).toHaveURL(/\/notes\/note-imported$/);
  expect(runtimeErrors).toEqual([]);
});

test("unclassified legacy replicas expose no copy action the backend would refuse", async ({
  page,
}) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await boot(page);

  await page
    .getByRole("button", { name: "Actions for Legacy shared item" })
    .click();
  const menu = page.getByRole("menu", {
    name: "Actions for Legacy shared item",
  });
  await expect(
    menu.getByRole("menuitem", { name: "Open shared item" }),
  ).toBeVisible();
  await expect(
    menu.getByRole("menuitem", { name: "Add a copy to Workspace…" }),
  ).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __orgImports?: unknown[] }).__orgImports ?? [],
    ),
  ).toEqual([]);
  expect(runtimeErrors).toEqual([]);
});

test("owned sources open the local original", async ({ page }) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await boot(page);

  await page.getByRole("button", { name: "Studio", exact: true }).click();
  await page.locator(".row-main", { hasText: "Studio planning" }).click();
  await expect(page).toHaveURL(/\/meeting\/meeting-local$/);
  expect(runtimeErrors).toEqual([]);
});

test("owned sources move the local original to a Workspace", async ({ page }) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await boot(page);

  await page.getByRole("button", { name: "Studio", exact: true }).click();
  await page
    .getByRole("button", { name: "Actions for Studio planning" })
    .click();
  await page
    .getByRole("menuitem", { name: "Move local original to Workspace…" })
    .click();
  const moveSheet = page.getByRole("dialog", {
    name: "Move “Studio planning”",
  });
  await expect(
    moveSheet.getByRole("button", { name: "Choose Unlocked private" }),
  ).toBeVisible();
  await moveSheet.getByRole("button", { name: "Choose Product" }).click();
  await moveSheet.getByRole("button", { name: "Move here", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __ownedMoves?: unknown[] }).__ownedMoves ??
          [],
      ),
    )
    .toEqual([{ meetingId: "meeting-local", folderId: "space-product", confirmedEncryptionBoundary: false }]);
  await expect(page).toHaveURL(/\/meeting\/meeting-local$/);
  expect(runtimeErrors).toEqual([]);
});

for (const viewport of [
  { width: 1280, height: 900 },
  { width: 760, height: 480 },
]) {
  test(`Shared Brains remains reachable and hit-testable at ${viewport.width}x${viewport.height}`, async ({
    page,
  }) => {
    const runtimeErrors = watchRuntimeErrors(page);
    await boot(page, viewport);
    const actions = page.getByRole("button", {
      name: "Actions for Customer review",
    });
    await actions.scrollIntoViewIfNeeded();
    const box = await actions.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.y).toBeGreaterThanOrEqual(0);
    expect(box!.y + box!.height).toBeLessThanOrEqual(viewport.height);
    await actions.click();
    const menu = page.getByRole("menu", {
      name: "Actions for Customer review",
    });
    await expect(menu).toBeVisible();
    await expect(
      menu.getByRole("menuitem", { name: "Add a copy to Workspace…" }),
    ).toBeVisible();
    expect(runtimeErrors).toEqual([]);
  });
}

test('privacy invalidation hides an in-flight copy and blocks duplicates until its write settles', async ({page}) => {
  await boot(page);
  await page.evaluate(() => {
    const host = window as any;
    const invoke = host.__TAURI_INTERNALS__.invoke.bind(host.__TAURI_INTERNALS__);
    host.__pendingCopies = 0;
    host.__TAURI_INTERNALS__.invoke = (cmd: string,args: any) => {
      if (cmd === 'add_org_item_to_container') {
        host.__pendingCopies++;
        return new Promise(resolve => {
          host.__finishPendingCopy = () => resolve({kind:'note',id:'note-imported'});
        });
      }
      return invoke(cmd,args);
    };
  });
  const openCopy = async () => {
    await page.getByRole('button',{name:'Actions for Research brief',exact:true}).click();
    await page.getByRole('menuitem',{name:'Add a copy to Workspace…',exact:true}).click();
  };
  await openCopy();
  const dialog = page.getByRole('dialog',{name:'Add a copy “Research brief”'});
  await dialog.getByRole('button',{name:'Choose Product',exact:true}).click();
  await dialog.getByRole('button',{name:'Add a copy here',exact:true}).click();
  await expect.poll(() => page.evaluate(() => (window as any).__pendingCopies)).toBe(1);
  await page.evaluate(() => (window as any).__demoEmit('murmur://reminder-visibility-invalidated',null));
  await expect(dialog).toHaveCount(0);
  await openCopy();
  await expect(dialog).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).__pendingCopies)).toBe(1);
  await page.evaluate(() => (window as any).__finishPendingCopy());
  await page.waitForTimeout(150);
  await expect(page).toHaveURL(/\/shared-brains$/);
  await expect(page.getByRole('tab', {name:/Research brief/})).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).__pendingCopies)).toBe(1);
});

test('a readable owned share with movable false does not expose its local Move action', async ({page}) => {
  await boot(page);
  await page.addInitScript(() => {
    const host = window as any;
    const invoke = host.__TAURI_INTERNALS__.invoke.bind(host.__TAURI_INTERNALS__);
    host.__TAURI_INTERNALS__.invoke = async (cmd: string,args: any) => {
      const result = await invoke(cmd,args);
      return cmd === 'list_org_items' ? result.map((item: any) => ({...item,
        ownedSource:item.ownedSource ? {...item.ownedSource,movable:false} : null})) : result;
    };
  });
  await page.reload();
  await page.getByRole('button',{name:'Studio',exact:true}).click();
  await page.getByRole('button',{name:'Actions for Studio planning',exact:true}).click();
  await expect(page.getByRole('menuitem',{name:'Move local original to Workspace…',exact:true})).toHaveCount(0);
  await expect(page.getByRole('menuitem',{name:'Open meeting',exact:true})).toBeVisible();
});
