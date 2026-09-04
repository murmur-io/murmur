import { expect, test } from "@playwright/test";
import { mockNotes } from "../notes/mock-invoke";

const ORG_ID = "11111111-1111-4111-8111-111111111111";
const DOC_VIEW_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const VIEW_LINK_ID = `${ORG_ID}:${DOC_VIEW_ID}`;
const CONFLICT_LINK_ID = `${ORG_ID}:cccccccc-cccc-4ccc-8ccc-cccccccccccc`;

const ORG = () => [
  {
    orgId: "11111111-1111-4111-8111-111111111111",
    name: "Acme",
    role: "member",
    memberCount: 3,
    consented: true,
    lastSeq: 4,
    itemCount: 0,
    receivedCount: 1,
    pendingShares: 0,
    contextEnabled: true,
  },
];

test("view-only received item has reverse Related but no edit or management controls", async ({
  page,
}) => {
  await mockNotes(page, {
    org_resolve_source: () => null,
    org_get_item: () => ({
      itemId: "item-1",
      docId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      linkId:
        "11111111-1111-4111-8111-111111111111:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      authorHint: "kasia",
      title: "Shared roadmap",
      createdAt: "2026-08-12T10:00:00Z",
      rev: 2,
      markdown: "# Shared roadmap\n\nPrivate org content.",
      access: "view",
      canEdit: false,
      canManage: false,
      editable: false,
    }),
    org_list_statuses: ORG,
    list_org_items: () => [
      {
        itemId: "item-1",
        title: "Shared roadmap",
        authorHint: "kasia",
        createdAt: "2026-08-12T10:00:00Z",
        seq: 4,
      },
    ],
    list_links: (args: unknown) => {
      const w = window as unknown as { __orgLinkReads?: unknown[] };
      (w.__orgLinkReads ??= []).push(args);
      return (w as typeof w & { __reverseLinked?: boolean }).__reverseLinked
        ? [
            {
              id: 91,
              direction: "out",
              otherKind: "note",
              otherId: "note-9",
              otherTitle: "Local follow-up",
              edgeType: "manual",
              createdBy: "user",
              status: "active",
              score: 1,
              createdAt: 1,
              manual: true,
            },
          ]
        : [];
    },
    get_related_picker_bootstrap: (args: {
      anchorKind: string;
      anchorId: string;
    }) => {
      const w = window as unknown as { __relatedBootstrapCalls?: unknown[] };
      (w.__relatedBootstrapCalls ??= []).push(args);
      return {
        spaces: [
          {
            id: "p-root",
            name: "Workspace",
            level: "project",
            emoji: null,
            locked: false,
            unlocked: false,
            linkable: true,
            groups: [],
            folders: [
              {
                id: "nf1",
                name: "Notes",
                level: "folder",
                emoji: null,
                locked: false,
                unlocked: false,
                linkable: true,
                groups: [{ kind: "note", total: 1 }],
                folders: [],
              },
            ],
          },
        ],
        unclassified: [],
        anchor: null,
      };
    },
    list_related_picker_items: (args: { kind: string; offset: number }) => ({
      kind: args.kind,
      offset: args.offset,
      items:
        args.kind === "note"
          ? [{ kind: "note", id: "note-9", title: "Local follow-up" }]
          : [],
      total: args.kind === "note" ? 1 : 0,
    }),
    search_related_picker: (args: {
      anchorKind: string;
      anchorId: string;
      query: string;
      offset: number;
      limit: number;
    }) => {
      const w = window as unknown as { __relatedSearchCalls?: unknown[] };
      (w.__relatedSearchCalls ??= []).push(args);
      const matches = args.query.trim().toLowerCase() === "local follow-up";
      return {
        offset: 0,
        hits: matches
          ? [
              {
                kind: "note",
                id: "note-9",
                title: "Local follow-up",
                breadcrumb: ["Workspace", "Notes"],
              },
            ]
          : [],
        total: matches ? 1 : 0,
      };
    },
    link_items: (args: unknown) => {
      const w = window as unknown as {
        __reverseLinkCalls?: unknown[];
        __reverseLinked?: boolean;
      };
      (w.__reverseLinkCalls ??= []).push(args);
      w.__reverseLinked = true;
      return null;
    },
    list_note_attachments: () => [],
    account_status: () => ({ loggedIn: true }),
  });

  await page.goto("/org-item/item-1");
  await expect(page.locator(".oi-title")).toHaveText("Shared roadmap");
  await expect(page.getByText("View only", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Edit", exact: true })).toHaveCount(0);
  await expect(page.locator(".oi-permissions")).toHaveCount(0);
  await expect(page.locator("app-connections")).toBeVisible();
  await expect.poll(async () =>
    page.evaluate(
      () =>
        (window as unknown as { __orgLinkReads?: unknown[] }).__orgLinkReads ?? [],
    ),
  ).toContainEqual({ kind: "org", id: VIEW_LINK_ID });

  const connections = page.locator("app-connections");
  await connections.getByRole("button", { name: "Link", exact: true }).click();
  const picker = page.getByRole("dialog", { name: "Add related" });
  await expect(picker).toBeVisible();
  await expect.poll(async () =>
    page.evaluate(
      () =>
        (
          window as unknown as { __relatedBootstrapCalls?: unknown[] }
        ).__relatedBootstrapCalls ?? [],
    ),
  ).toContainEqual({ anchorKind: "org", anchorId: VIEW_LINK_ID });
  await picker.getByPlaceholder("Search every Space…").fill("Local follow-up");
  await picker
    .locator('[data-row="h:note:note-9"] .rhp-row-main')
    .click();
  await expect(connections.getByText("Local follow-up")).toBeVisible();
  await expect.poll(async () =>
    page.evaluate(
      () =>
        (window as unknown as { __reverseLinkCalls?: unknown[] })
          .__reverseLinkCalls ?? [],
    ),
  ).toContainEqual({
    srcKind: "org",
    srcId: VIEW_LINK_ID,
    dstKind: "note",
    dstId: "note-9",
  });
  await expect.poll(async () =>
    page.evaluate(
      () =>
        (window as unknown as { __relatedSearchCalls?: unknown[] })
          .__relatedSearchCalls ?? [],
    ),
  ).toContainEqual({
    anchorKind: "org",
    anchorId: VIEW_LINK_ID,
    query: "Local follow-up",
    offset: 0,
    limit: 30,
  });
});

test("edit conflict opens the stable latest head across an in-flight sync event", async ({
  page,
}) => {
  await mockNotes(page, {
    org_resolve_source: () => null,
    org_get_item: (args: { itemId: string }) => {
      const w = window as unknown as {
        __orgItemReads?: string[];
        __recoveryOps?: string[];
        __orgSyncStarted?: boolean;
        __orgSyncReleased?: boolean;
        __heldSyncReads?: string[];
      };
      (w.__orgItemReads ??= []).push(args.itemId);
      const duringHeldSync =
        w.__orgSyncStarted === true && w.__orgSyncReleased !== true;
      if (duringHeldSync) {
        (w.__heldSyncReads ??= []).push(args.itemId);
      }
      if (
        args.itemId ===
        "11111111-1111-4111-8111-111111111111:cccccccc-cccc-4ccc-8ccc-cccccccccccc"
      ) {
        (w.__recoveryOps ??= []).push("get-stable-head");
      }
      const latest = {
        itemId: "item-latest",
        docId: "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        linkId:
          "11111111-1111-4111-8111-111111111111:cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        authorHint: "sam",
        title: "Authoritative plan",
        createdAt: "2026-08-12T10:00:00Z",
        rev: 4,
        markdown: "# Authoritative body",
        access: "edit",
        canEdit: true,
        canManage: false,
      };
      if (
        args.itemId ===
          "11111111-1111-4111-8111-111111111111:cccccccc-cccc-4ccc-8ccc-cccccccccccc" ||
        args.itemId === "item-latest"
      ) {
        return latest;
      }
      if (args.itemId === "item-conflict" && duringHeldSync) {
        return null;
      }
      if (args.itemId !== "item-conflict") {
        return null;
      }
      return {
        ...latest,
        itemId: "item-conflict",
        title: "Concurrent plan",
        rev: 3,
        markdown: "# Old body",
      };
    },
    org_update_item: () => {
      const w = window as unknown as { __directEditWrites?: number };
      w.__directEditWrites = (w.__directEditWrites ?? 0) + 1;
      throw new Error(
        "storage error: [org-edit-conflict] expected revision is stale",
      );
    },
    org_sync_now: (args: { orgId: string }) => {
      const w = window as unknown as {
        __orgSyncs?: string[];
        __recoveryOps?: string[];
        __orgSyncStarted?: boolean;
        __orgSyncReleased?: boolean;
        __releaseOrgSync?: () => void;
      };
      (w.__orgSyncs ??= []).push(args.orgId);
      (w.__recoveryOps ??= []).push("sync-org");
      w.__orgSyncStarted = true;
      return new Promise((resolve) => {
        w.__releaseOrgSync = () => {
          if (w.__orgSyncReleased) {
            return;
          }
          w.__orgSyncReleased = true;
          (w.__recoveryOps ??= []).push("finish-sync");
          resolve({
            pulled: 1,
            ingested: 1,
            tombstoned: 1,
            lastSeq: 5,
            ftsOnly: false,
            errors: [],
          });
        };
      });
    },
    share_document_to_org: () => {
      const w = window as unknown as { __shareWrites?: number };
      w.__shareWrites = (w.__shareWrites ?? 0) + 1;
      return null;
    },
    share_meeting_to_org: () => {
      const w = window as unknown as { __shareWrites?: number };
      w.__shareWrites = (w.__shareWrites ?? 0) + 1;
      return null;
    },
    org_set_item_access: () => {
      const w = window as unknown as { __accessWrites?: number };
      w.__accessWrites = (w.__accessWrites ?? 0) + 1;
      return null;
    },
    org_list_statuses: ORG,
    list_org_items: () => [],
    list_links: () => [],
    list_note_attachments: (args: { ownerId: string }) => {
      const w = window as unknown as { __attachmentReads?: string[] };
      (w.__attachmentReads ??= []).push(args.ownerId);
      return [];
    },
    account_status: () => ({ loggedIn: true }),
  });

  // Exercise the tab-eviction branch too: a stale item-id revalidation would
  // call closeTab and remove this active entry before recovery can finish.
  await page.addInitScript(() => {
    localStorage.setItem(
      "murmur.tabs.v1",
      JSON.stringify({
        tabs: [
          {
            id: "org-item:item-conflict",
            kind: "org-item",
            entityId: "item-conflict",
            title: "Conflict tab",
            route: ["/org-item", "item-conflict"],
          },
        ],
        activeTabId: "org-item:item-conflict",
      }),
    );
  });

  await page.goto("/org-item/item-conflict");
  const edit = page.getByRole("button", { name: "Edit", exact: true });
  await expect(edit).toBeVisible();
  const activeTab = page.locator(".tab-item.active");
  await expect(activeTab.locator(".tab-label")).toHaveText("Concurrent plan");
  await expect(page.locator(".oi-permissions")).toHaveCount(0);
  await edit.click();
  const titleDraft = page.getByRole("textbox", { name: "Note title" });
  const draft = page.getByRole("textbox", { name: "Note content (markdown)" });
  await titleDraft.fill("My exact draft title");
  await draft.fill("# My unsaved draft\n\nKeep this text.");
  await page.getByRole("button", { name: "Save", exact: true }).click();

  const conflict = page.getByTestId("org-edit-conflict");
  await expect(titleDraft).toHaveValue("My exact draft title");
  await expect(draft).toHaveValue("# My unsaved draft\n\nKeep this text.");
  await expect(conflict).toContainText("Shared note changed elsewhere");
  await expect(conflict).toContainText(
    "Your draft is still here. Open the latest version before trying again.",
  );
  await expect(conflict).not.toContainText("storage error");
  await expect(conflict).not.toContainText("org-edit-conflict");
  await expect(conflict.getByRole("button", { name: "Open latest" })).toBeVisible();
  await expect(page).toHaveURL(/\/org-item\/item-conflict$/);

  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __directEditWrites?: number })
          .__directEditWrites ?? 0,
    ),
  ).toBe(1);
  expect(
    await page.evaluate(
      () => (window as unknown as { __shareWrites?: number }).__shareWrites ?? 0,
    ),
  ).toBe(0);
  expect(
    await page.evaluate(
      () => (window as unknown as { __accessWrites?: number }).__accessWrites ?? 0,
    ),
  ).toBe(0);
  expect(
    await page.evaluate(
      () => (window as unknown as { __orgSyncs?: string[] }).__orgSyncs ?? [],
    ),
  ).toEqual([]);

  await conflict.getByRole("button", { name: "Open latest" }).click();
  await expect.poll(async () =>
    page.evaluate(
      () => (window as unknown as { __orgSyncs?: string[] }).__orgSyncs ?? [],
    ),
  ).toEqual([ORG_ID]);
  await expect(
    conflict.getByRole("button", { name: "Opening…" }),
  ).toBeDisabled();

  // The real sync command emits this while its invoke Promise is still held.
  // At this point the immutable route id is stale and deliberately returns
  // null; only the stable document identity can resolve the successor.
  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://org-feed-updated", { orgsChanged: 1 });
  });
  await expect.poll(async () =>
    page.evaluate(
      () =>
        (window as unknown as { __heldSyncReads?: string[] })
          .__heldSyncReads ?? [],
    ),
  ).toEqual([CONFLICT_LINK_ID]);

  // Revalidation found a live successor, but opening/edit state stays frozen
  // until the explicit sync operation completes. No false withdrawal may
  // evict the view or its active tab, and neither draft may be touched.
  await expect(page).toHaveURL(/\/org-item\/item-conflict$/);
  await expect(page.getByTestId("org-item-removed")).toHaveCount(0);
  await expect(activeTab).toHaveCount(1);
  await expect(activeTab.locator(".tab-label")).toHaveText("Concurrent plan");
  await expect(titleDraft).toHaveValue("My exact draft title");
  await expect(draft).toHaveValue("# My unsaved draft\n\nKeep this text.");
  await expect(conflict).toBeVisible();
  await expect(conflict).not.toContainText("storage error");
  await expect(conflict).not.toContainText("org-edit-conflict");
  expect(
    await page.evaluate(
      () => (window as unknown as { __shareWrites?: number }).__shareWrites ?? 0,
    ),
  ).toBe(0);
  expect(
    await page.evaluate(
      () => (window as unknown as { __accessWrites?: number }).__accessWrites ?? 0,
    ),
  ).toBe(0);

  await page.evaluate(() => {
    (
      window as unknown as { __releaseOrgSync?: () => void }
    ).__releaseOrgSync?.();
  });

  await expect(page).toHaveURL(/\/org-item\/item-latest$/);
  await expect(page.locator(".oi-title")).toHaveText("Authoritative plan");
  await expect(page.locator(".oi-rev")).toHaveText("revision 4");
  await expect(page.locator(".oi-body")).toContainText("Authoritative body");
  await expect.poll(async () =>
    page.evaluate(
      () => (window as unknown as { __orgItemReads?: string[] }).__orgItemReads ?? [],
    ),
  ).toContain(CONFLICT_LINK_ID);
  await expect.poll(async () =>
    page.evaluate(() => {
      const reads =
        (window as unknown as { __attachmentReads?: string[] })
          .__attachmentReads ?? [];
      return reads.at(-1);
    }),
  ).toBe("item-latest");
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __attachmentReads?: string[] })
          .__attachmentReads ?? [],
    ),
  ).not.toContain(CONFLICT_LINK_ID);
  expect(
    await page.evaluate(
      () => (window as unknown as { __recoveryOps?: string[] }).__recoveryOps ?? [],
    ),
  ).toEqual([
    "sync-org",
    "get-stable-head",
    "finish-sync",
    "get-stable-head",
  ]);
  expect(
    await page.evaluate(
      () => (window as unknown as { __shareWrites?: number }).__shareWrites ?? 0,
    ),
  ).toBe(0);
  expect(
    await page.evaluate(
      () => (window as unknown as { __accessWrites?: number }).__accessWrites ?? 0,
    ),
  ).toBe(0);
});

test("source-share conflict stops silent republish and opens the latest head", async ({
  page,
}) => {
  await mockNotes(page, {
    account_status: () => ({
      loggedIn: true,
      email: "you@example.com",
      unlockedForSharing: true,
      shareConsented: true,
      serverConfigured: true,
      biometricUnlockAvailable: true,
    }),
    org_list_statuses: ORG,
    org_live_shares_for_source: () => {
      const w = window as unknown as { __orgLiveReads?: number };
      w.__orgLiveReads = (w.__orgLiveReads ?? 0) + 1;
      return [
        {
          orgId: "11111111-1111-4111-8111-111111111111",
          itemId: "item-current",
          rev: 4,
          access: "edit",
          conflicted: true,
        },
      ];
    },
    share_document_to_org: () => {
      const w = window as unknown as { __silentReshares?: number };
      w.__silentReshares = (w.__silentReshares ?? 0) + 1;
      return null;
    },
    org_set_item_access: () => {
      const w = window as unknown as { __accessWrites?: number };
      w.__accessWrites = (w.__accessWrites ?? 0) + 1;
      return null;
    },
    org_get_item: () => ({
      itemId: "item-current",
      docId: "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
      linkId:
        "11111111-1111-4111-8111-111111111111:dddddddd-dddd-4ddd-8ddd-dddddddddddd",
      authorHint: "alex",
      title: "Latest shared plan",
      createdAt: "2026-08-13T10:00:00Z",
      rev: 5,
      markdown: "# Latest shared plan",
      access: "edit",
      canEdit: true,
      canManage: false,
    }),
    org_resolve_source: () => null,
    list_org_items: () => [],
    list_links: () => [],
  });

  await page.goto("/notes/n1");
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: /Share/ }).click();

  const conflict = page.getByTestId("org-share-conflict");
  await expect(conflict).toContainText(
    "Shared copy changed elsewhere. Automatic updates stopped to protect the latest version.",
  );
  await expect(conflict.getByRole("button", { name: "Open latest" })).toBeVisible();
  await expect(page.locator(".org-access-options")).toHaveCount(0);
  await expect.poll(async () =>
    page.evaluate(
      () =>
        (window as unknown as { __orgLiveReads?: number }).__orgLiveReads ?? 0,
    ),
  ).toBe(1);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __silentReshares?: number }).__silentReshares ?? 0,
    ),
  ).toBe(0);
  expect(
    await page.evaluate(
      () => (window as unknown as { __accessWrites?: number }).__accessWrites ?? 0,
    ),
  ).toBe(0);

  await conflict.getByRole("button", { name: "Open latest" }).click();
  await expect(page).toHaveURL(/\/org-item\/item-current$/);
  await expect(page.locator(".oi-title")).toHaveText("Latest shared plan");
  await expect(page.getByRole("button", { name: "Edit", exact: true })).toBeVisible();
  await expect(page.locator(".oi-permissions")).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __silentReshares?: number }).__silentReshares ?? 0,
    ),
  ).toBe(0);
});

test("editor can edit while only a manager can change member access", async ({
  page,
}) => {
  await mockNotes(page, {
    org_resolve_source: () => null,
    org_get_item: () => {
      const access =
        (window as unknown as { __orgAccess?: "view" | "edit" }).__orgAccess ??
        "view";
      return {
        itemId: "item-2",
        docId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        linkId:
          "11111111-1111-4111-8111-111111111111:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        authorHint: "me",
        title: "Managed plan",
        createdAt: "2026-08-12T10:00:00Z",
        rev: 1,
        markdown: "# Managed plan",
        access,
        canEdit: true,
        canManage: true,
        editable: true,
      };
    },
    org_set_item_access: (args: { itemId: string; access: "view" | "edit" }) => {
      const w = window as unknown as {
        __orgAccess?: "view" | "edit";
        __orgAccessCalls?: unknown[];
      };
      w.__orgAccess = args.access;
      (w.__orgAccessCalls ??= []).push(args);
      return null;
    },
    org_list_statuses: ORG,
    list_org_items: () => [],
    list_links: () => [],
    list_note_attachments: () => [],
    account_status: () => ({ loggedIn: true }),
  });

  await page.goto("/org-item/item-2");
  await expect(page.getByRole("button", { name: "Edit", exact: true })).toBeVisible();
  const editAccess = page.locator(".oi-permissions").getByRole("button", {
    name: "Can edit",
    exact: true,
  });
  await editAccess.click();
  await expect(editAccess).toHaveAttribute("aria-pressed", "true");
  await expect.poll(async () =>
    page.evaluate(
      () =>
        (window as unknown as { __orgAccessCalls?: unknown[] }).__orgAccessCalls ??
        [],
    ),
  ).toEqual([{ itemId: "item-2", access: "edit" }]);
});
