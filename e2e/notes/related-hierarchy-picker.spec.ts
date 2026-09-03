import { expect, test, type Page } from "@playwright/test";

import { mockNotes } from "./mock-invoke";

const picker = (page: Page) =>
  page.getByRole("dialog", { name: "Add related" });

async function openPicker(page: Page): Promise<void> {
  await page
    .locator("app-connections")
    .getByRole("button", { name: "Link", exact: true })
    .click();
  await expect(picker(page)).toBeVisible();
}

async function emit(page: Page, event: string): Promise<void> {
  await page.evaluate((name) => {
    (
      window as unknown as {
        __demoEmit: (eventName: string, payload: unknown) => void;
      }
    ).__demoEmit(name, null);
  }, event);
}

/**
 * Add a page-side gate around org-feed listeners registered while armed. Existing listeners boot
 * normally; a test arms this only after navigation, immediately before mounting the picker.
 */
async function installPickerOrgListenerGate(
  page: Page,
  initialMode: "pass" | "hold" | "reject" = "pass",
): Promise<void> {
  await page.addInitScript((mode) => {
    const target = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (command: string, args?: unknown) => Promise<unknown>;
      };
      __pickerOrgListenerMode?: "pass" | "hold" | "reject";
      __pickerOrgListenerHeld?: boolean;
      __pickerOrgListenerRejected?: boolean;
      __releasePickerOrgListener?: () => void;
    };
    const invoke = target.__TAURI_INTERNALS__.invoke.bind(
      target.__TAURI_INTERNALS__,
    );
    target.__pickerOrgListenerMode = mode;
    target.__TAURI_INTERNALS__.invoke = (command, args) => {
      const event =
        typeof args === "object" && args !== null && "event" in args
          ? String((args as { event: unknown }).event)
          : "";
      if (
        command !== "plugin:event|listen" ||
        event !== "murmur://org-feed-updated"
      ) {
        return invoke(command, args);
      }
      const mode = target.__pickerOrgListenerMode ?? "pass";
      if (mode === "reject") {
        target.__pickerOrgListenerRejected = true;
        return Promise.reject(new Error("picker org listener rejected"));
      }
      if (mode === "hold") {
        target.__pickerOrgListenerHeld = true;
        return new Promise((resolve, reject) => {
          const previousRelease = target.__releasePickerOrgListener;
          target.__releasePickerOrgListener = () => {
            target.__pickerOrgListenerMode = "pass";
            target.__pickerOrgListenerHeld = false;
            previousRelease?.();
            invoke(command, args).then(resolve, reject);
          };
        });
      }
      return invoke(command, args);
    };
  }, initialMode);
}

function sharedWorkspaceFixture() {
  return {
    spaces: [
      {
        containerId: "shared-space",
        orgId: "org-a",
        orgName: "Atlas Org",
        name: "Shared Atlas",
        level: "space",
        access: "view",
        authorHint: "kasia",
        folders: [],
        items: [
          {
            itemId: "shared-rev-4",
            docId: "shared-doc",
            title: "Shared launch note",
            kind: "document",
            authorHint: "kasia",
            createdAt: "2026-09-03T10:00:00Z",
            orgId: "org-a",
            orgName: "Atlas Org",
            access: "view",
            position: 0,
          },
        ],
        position: 0,
      },
    ],
    sharedBrains: {
      orgId: "shared",
      orgName: "Shared",
      name: "Shared Brains",
      level: "virtual",
      access: "view",
      authorHint: "",
      folders: [],
      items: [],
      position: 1,
    },
  };
}

test("roving keyboard traverses Current, Linked, and locked rows to the next linkable place", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => [
      {
        id: 71,
        direction: "out",
        otherKind: "note",
        otherId: "n-linked",
        otherTitle: "Already linked",
        edgeType: "manual",
        createdBy: "user",
        status: "active",
        score: 1,
        createdAt: 1_720_000_000,
        manual: true,
      },
    ],
    get_backlinks: () => [],
    get_related_picker_bootstrap: () => ({
      spaces: [
        {
          id: "p-current",
          name: "Current Space",
          level: "project",
          emoji: null,
          locked: false,
          unlocked: false,
          linkable: true,
          groups: [{ kind: "note", total: 2 }],
          folders: [],
        },
        {
          id: "p-locked",
          name: "Locked Space",
          level: "project",
          emoji: null,
          locked: true,
          unlocked: false,
          linkable: false,
          groups: [],
          folders: [],
        },
        {
          id: "p-later",
          name: "Later Space",
          level: "project",
          emoji: null,
          locked: false,
          unlocked: false,
          linkable: true,
          groups: [],
          folders: [],
        },
      ],
      unclassified: [],
      anchor: {
        kind: "note",
        containerId: "p-current",
        path: ["p-current"],
        index: 0,
        offset: 0,
        items: [
          { kind: "note", id: "n1", title: "Current note" },
          { kind: "note", id: "n-linked", title: "Already linked" },
        ],
        total: 2,
      },
    }),
  });

  await page.goto("/notes/n1");
  await openPicker(page);
  const dialog = picker(page);
  const current = dialog.locator('[data-row="i:note:n1"] .rhp-row-main');
  const linked = dialog.locator('[data-row="i:note:n-linked"] .rhp-row-main');
  const locked = dialog.locator('[data-row="c:p-locked"] .rhp-row-main');
  const later = dialog.locator('[data-row="c:p-later"] .rhp-row-main');

  await expect(current).toHaveAttribute("aria-disabled", "true");
  await current.focus();
  await expect(current).toBeFocused();
  await current.press("Enter");
  await expect(page.getByRole("alertdialog")).toHaveCount(0);

  await current.press("ArrowDown");
  await expect(linked).toBeFocused();
  await expect(linked).toHaveAttribute("aria-disabled", "true");
  await linked.press("ArrowDown");
  await expect(locked).toBeFocused();
  await expect(locked).toHaveAttribute("aria-disabled", "true");
  await locked.press("ArrowDown");
  await expect(later).toBeFocused();
  await expect(later).not.toHaveAttribute("aria-disabled", "true");
  await expect(later.locator("xpath=..")).not.toHaveAttribute(
    "aria-expanded",
    /.+/,
  );
  await expect(
    dialog.getByRole("button", { name: "Expand Later Space" }),
  ).toHaveCount(0);
  await later.press("ArrowRight");
  await expect(later).toBeFocused();
  await expect(
    dialog.getByRole("button", { name: "Link Space Later Space" }),
  ).toBeVisible();
});

test("opens on the centred current path, pages with anchor-gated args, searches breadcrumbs, and restores hierarchy scroll", async ({
  page,
}) => {
  await mockNotes(page, {
    get_related_picker_bootstrap: (args: unknown) => {
      const w = window as unknown as { __pickerCalls?: unknown[] };
      (w.__pickerCalls ??= []).push({
        cmd: "get_related_picker_bootstrap",
        args,
      });
      const items = Array.from({ length: 24 }, (_, index) => {
        const absolute = 40 + index;
        return absolute === 50
          ? { kind: "note", id: "n1", title: "Current deep note" }
          : {
              kind: "note",
              id: `deep-${absolute}`,
              title: `Deep note ${absolute}`,
            };
      });
      return {
        spaces: [
          {
            id: "p-product",
            name: "Product",
            level: "project",
            emoji: null,
            locked: false,
            unlocked: false,
            linkable: true,
            groups: [],
            folders: [
              {
                id: "f-roadmap",
                name: "Roadmap",
                level: "folder",
                emoji: null,
                locked: false,
                unlocked: false,
                linkable: true,
                groups: [
                  { kind: "note", total: 100 },
                  { kind: "meeting", total: 1 },
                  { kind: "document", total: 1 },
                ],
                folders: [],
              },
            ],
          },
          {
            id: "p-private",
            name: "Private",
            level: "project",
            emoji: null,
            locked: true,
            unlocked: false,
            linkable: false,
            groups: [],
            folders: [],
          },
        ],
        unclassified: [
          { kind: "meeting", total: 2 },
          { kind: "note", total: 1 },
        ],
        anchor: {
          kind: "note",
          containerId: "f-roadmap",
          path: ["p-product", "f-roadmap"],
          index: 50,
          offset: 40,
          items,
          total: 100,
        },
      };
    },
    list_related_picker_items: (args: { kind: string; offset: number }) => {
      const w = window as unknown as { __pickerCalls?: unknown[] };
      (w.__pickerCalls ??= []).push({
        cmd: "list_related_picker_items",
        args,
      });
      return {
        kind: args.kind,
        offset: args.offset,
        items: Array.from({ length: 24 }, (_, index) => ({
          kind: args.kind,
          id: `earlier-${args.offset + index}`,
          title: `Earlier note ${args.offset + index}`,
        })),
        total: 100,
      };
    },
    search_related_picker: (args: unknown) => {
      const w = window as unknown as { __pickerCalls?: unknown[] };
      (w.__pickerCalls ??= []).push({ cmd: "search_related_picker", args });
      return {
        offset: 0,
        hits: [
          {
            kind: "note",
            id: "n-roadmap-hit",
            title: "Roadmap decision",
            breadcrumb: ["Product", "Roadmap"],
          },
        ],
        total: 1,
      };
    },
    list_links: () => [],
    get_backlinks: () => [],
  });

  await page.goto("/notes/n1");
  await openPicker(page);

  const dialog = picker(page);
  await expect(dialog.getByText("Opened at")).toBeVisible();
  await expect(
    dialog.getByText("Product / Roadmap", { exact: true }),
  ).toBeVisible();
  await expect(
    dialog.getByText("Current deep note", { exact: true }),
  ).toBeVisible();
  await expect(dialog.getByText("Current", { exact: true })).toBeVisible();
  await expect(dialog.getByText("Tasks", { exact: true })).toHaveCount(0);
  await expect(dialog.getByText("Dashboards", { exact: true })).toHaveCount(0);
  await expect(dialog.getByText("Private", { exact: true })).toBeVisible();
  await expect(dialog.getByText(/Unlock it in the sidebar/)).toBeVisible();

  const tree = dialog.locator(".rhp-tree");
  const current = dialog.locator(".rhp-row.is-current");
  const centred = await Promise.all([
    tree.boundingBox(),
    current.boundingBox(),
  ]);
  expect(centred[0]).not.toBeNull();
  expect(centred[1]).not.toBeNull();
  const treeMid = centred[0]!.y + centred[0]!.height / 2;
  const currentMid = centred[1]!.y + centred[1]!.height / 2;
  expect(Math.abs(treeMid - currentMid)).toBeLessThan(
    centred[0]!.height * 0.35,
  );

  await dialog.getByRole("button", { name: "Load earlier" }).click();
  await expect(
    dialog.getByText("Earlier note 16", { exact: true }),
  ).toBeVisible();

  await tree.evaluate((element) => {
    element.scrollTop = 73;
  });
  const search = dialog.getByPlaceholder("Search every Space…");
  await search.fill("roadmap");
  await expect(
    dialog.getByText("Roadmap decision", { exact: true }),
  ).toBeVisible();
  await expect(
    dialog
      .locator('[data-row="h:note:n-roadmap-hit"]')
      .getByText("Product / Roadmap", { exact: true }),
  ).toBeVisible();
  await dialog.getByRole("button", { name: "Clear search" }).click();
  await expect(
    dialog.getByText("Current deep note", { exact: true }),
  ).toBeVisible();
  await expect
    .poll(() => tree.evaluate((element) => element.scrollTop))
    .toBe(73);

  const calls = await page.evaluate(
    () =>
      (window as unknown as { __pickerCalls?: unknown[] }).__pickerCalls ?? [],
  );
  expect(calls).toContainEqual({
    cmd: "get_related_picker_bootstrap",
    args: { anchorKind: "note", anchorId: "n1" },
  });
  expect(calls).toContainEqual({
    cmd: "list_related_picker_items",
    args: {
      anchorKind: "note",
      anchorId: "n1",
      containerId: "f-roadmap",
      kind: "note",
      offset: 16,
      limit: 24,
    },
  });
  expect(calls).toContainEqual({
    cmd: "search_related_picker",
    args: {
      anchorKind: "note",
      anchorId: "n1",
      query: "roadmap",
      offset: 0,
      limit: 30,
    },
  });
});

test("searches visible local places and Shared breadcrumbs without surfacing locked scopes", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => [],
    get_backlinks: () => [],
    get_related_picker_bootstrap: () => ({
      spaces: [
        {
          id: "p-product",
          name: "Product",
          level: "project",
          emoji: null,
          locked: false,
          unlocked: false,
          linkable: true,
          groups: [],
          folders: [
            {
              id: "f-roadmap",
              name: "Roadmap",
              level: "folder",
              emoji: null,
              locked: false,
              unlocked: false,
              linkable: true,
              groups: [{ kind: "note", total: 1 }],
              folders: [],
            },
            {
              id: "f-private-roadmap",
              name: "Partner Roadmap Secret",
              level: "folder",
              emoji: null,
              locked: true,
              unlocked: false,
              linkable: false,
              groups: [],
              folders: [],
            },
          ],
        },
      ],
      unclassified: [],
      anchor: {
        kind: "note",
        containerId: "f-roadmap",
        path: ["p-product", "f-roadmap"],
        index: 0,
        offset: 0,
        items: [{ kind: "note", id: "n1", title: "Current note" }],
        total: 1,
      },
    }),
    search_related_picker: () => ({ offset: 0, hits: [], total: 0 }),
    link_items: (args: unknown) => {
      const target = window as unknown as {
        __hierarchySearchLinkCalls?: unknown[];
      };
      (target.__hierarchySearchLinkCalls ??= []).push(args);
      return null;
    },
    list_shared_workspace: () => ({
      spaces: [
        {
          containerId: "shared-atlas",
          orgId: "org-a",
          orgName: "Atlas Org",
          name: "Shared Atlas",
          level: "space",
          access: "view",
          authorHint: "kasia",
          folders: [
            {
              containerId: "partner-roadmap",
              orgId: "org-a",
              orgName: "Atlas Org",
              name: "Partner Roadmap",
              level: "folder",
              access: "view",
              authorHint: "kasia",
              folders: [],
              items: [
                {
                  itemId: "shared-launch-rev",
                  docId: "shared-launch-doc",
                  title: "Launch brief",
                  kind: "document",
                  authorHint: "kasia",
                  createdAt: "2026-09-04T10:00:00Z",
                  orgId: "org-a",
                  orgName: "Atlas Org",
                  access: "view",
                  position: 0,
                },
              ],
              position: 0,
            },
          ],
          items: [],
          position: 0,
        },
      ],
      sharedBrains: {
        orgId: "shared",
        orgName: "Shared",
        name: "Shared Brains",
        level: "virtual",
        access: "view",
        authorHint: "",
        folders: [],
        items: [],
        position: 1,
      },
    }),
  });

  await page.goto("/notes/n1");
  await openPicker(page);
  const dialog = picker(page);
  const search = dialog.getByPlaceholder("Search every Space…");

  // The folder's own name is only part of the query: the FULL local breadcrumb is searchable.
  await search.fill("Product / Roadmap");
  const localPlace = dialog.locator('[data-row="h:container:f-roadmap"]');
  await expect(localPlace).toBeVisible();
  await expect(localPlace.getByText("Contains current item")).toBeVisible();
  await expect(
    localPlace.getByRole("button", { name: "Link folder Roadmap" }),
  ).toBeVisible();
  await expect(dialog.getByText("1 result", { exact: true })).toBeVisible();

  // Search results retain listbox keyboard semantics and reuse the existing one-edge confirmation.
  await search.press("ArrowDown");
  await expect(localPlace.locator(".rhp-row-main")).toBeFocused();
  await page.keyboard.press("Enter");
  const confirm = page.getByRole("alertdialog");
  await expect(confirm).toContainText("one stable relation");
  await expect(confirm).toContainText("Product / Roadmap");
  await confirm.getByRole("button", { name: "Cancel" }).click();

  // A Shared FOLDER name finds the stable leaf inside it, never the disclosure-only container.
  await search.fill("Partner Roadmap");
  await expect(dialog.getByText("Launch brief", { exact: true })).toBeVisible();
  await expect(
    dialog.getByText("Shared Atlas / Partner Roadmap", { exact: true }),
  ).toBeVisible();
  await expect(
    dialog.getByRole("button", { name: /Link (Space|folder) Partner Roadmap/ }),
  ).toHaveCount(0);
  await expect(
    dialog.getByText("Partner Roadmap Secret", { exact: true }),
  ).toHaveCount(0);
  await expect(dialog.getByText("1 result", { exact: true })).toBeVisible();

  // Confirming the SEARCH result writes exactly one relation to the stable container id.
  await search.fill("Product / Roadmap");
  await dialog
    .locator('[data-row="h:container:f-roadmap"]')
    .getByRole("button", { name: "Link folder Roadmap" })
    .click();
  await page
    .getByRole("alertdialog")
    .getByRole("button", { name: "Link folder" })
    .click();
  await expect(dialog).toHaveCount(0);
  const linkCalls = await page.evaluate(
    () =>
      (window as unknown as { __hierarchySearchLinkCalls?: unknown[] })
        .__hierarchySearchLinkCalls ?? [],
  );
  expect(linkCalls).toEqual([
    {
      srcKind: "note",
      srcId: "n1",
      dstKind: "container",
      dstId: "f-roadmap",
    },
  ]);
});

test("keeps disclosure separate from one confirmed container link and links Shared leaves only", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => [],
    get_backlinks: () => [],
    list_shared_workspace: () => ({
      spaces: [
        {
          containerId: "shared-space",
          orgId: "org-a",
          orgName: "Atlas Org",
          name: "Shared Atlas",
          level: "space",
          access: "view",
          authorHint: "kasia",
          folders: [],
          items: [
            {
              itemId: "shared-rev-4",
              docId: "shared-doc",
              title: "Shared launch note",
              kind: "document",
              authorHint: "kasia",
              createdAt: "2026-09-03T10:00:00Z",
              orgId: "org-a",
              orgName: "Atlas Org",
              access: "view",
              position: 0,
            },
          ],
          position: 0,
        },
      ],
      sharedBrains: {
        orgId: "shared",
        orgName: "Shared",
        name: "Shared Brains",
        level: "virtual",
        access: "view",
        authorHint: "",
        folders: [],
        items: [],
        position: 1,
      },
    }),
    link_items: (args: unknown) => {
      const w = window as unknown as { __hierarchyLinkCalls?: unknown[] };
      (w.__hierarchyLinkCalls ??= []).push(args);
      return null;
    },
  });

  await page.goto("/notes/n1");
  await openPicker(page);
  const dialog = picker(page);

  const notesFolderRow = dialog.locator('[data-row="c:nf1"]');
  await expect(notesFolderRow.getByText("Contains current item")).toBeVisible();
  await notesFolderRow.locator(".rhp-row-main").click();
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
  await notesFolderRow
    .getByRole("button", { name: "Link folder Notes" })
    .click();
  const confirm = page.getByRole("alertdialog");
  await expect(confirm).toContainText("one stable relation");
  await expect(confirm.getByRole("button", { name: "Cancel" })).toBeFocused();
  await page.keyboard.press("Shift+Tab");
  await expect(
    confirm.getByRole("button", { name: "Link folder" }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(
    notesFolderRow.getByRole("button", { name: "Link folder Notes" }),
  ).toBeFocused();

  await notesFolderRow
    .getByRole("button", { name: "Link folder Notes" })
    .click();
  await confirm.getByRole("button", { name: "Link folder" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(
    page
      .locator("app-connections")
      .getByRole("button", { name: "Link", exact: true }),
  ).toBeFocused();

  await openPicker(page);
  const sharedDialog = picker(page);
  const emptySharedRoot = sharedDialog.locator('[data-row="s:shared:root"]');
  await expect(emptySharedRoot).not.toHaveAttribute("aria-expanded", /.+/);
  await expect(
    sharedDialog.getByRole("button", { name: "Expand Shared Brains" }),
  ).toHaveCount(0);
  await sharedDialog
    .getByRole("button", { name: "Expand Shared Atlas" })
    .click();
  await expect(
    sharedDialog.getByRole("button", {
      name: /Link (Space|folder) Shared Atlas/,
    }),
  ).toHaveCount(0);
  await sharedDialog
    .locator('[data-row="si:shared-rev-4"] .rhp-row-main')
    .click();

  const calls = await page.evaluate(
    () =>
      (window as unknown as { __hierarchyLinkCalls?: unknown[] })
        .__hierarchyLinkCalls ?? [],
  );
  expect(calls).toEqual([
    {
      srcKind: "note",
      srcId: "n1",
      dstKind: "container",
      dstId: "nf1",
    },
    {
      srcKind: "note",
      srcId: "n1",
      dstKind: "org",
      dstId: "org-a:shared-doc",
    },
  ]);
});

test("waits for its org-feed listener before reading Shared and drops a late reply after revocation", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => [],
    get_backlinks: () => [],
    list_shared_workspace: () => {
      const target = window as unknown as {
        __pickerSharedFixture: unknown;
        __pickerSharedReadCalls?: number;
        __holdPickerSharedReply?: boolean;
        __resolvePickerSharedReply?: () => void;
      };
      target.__pickerSharedReadCalls =
        (target.__pickerSharedReadCalls ?? 0) + 1;
      if (target.__holdPickerSharedReply) {
        return new Promise((resolve) => {
          target.__resolvePickerSharedReply = () =>
            resolve(target.__pickerSharedFixture);
        });
      }
      return target.__pickerSharedFixture;
    },
  });
  await installPickerOrgListenerGate(page);
  await page.addInitScript((fixture) => {
    (
      window as unknown as { __pickerSharedFixture: unknown }
    ).__pickerSharedFixture = fixture;
  }, sharedWorkspaceFixture());

  await page.goto("/notes/n1");
  // Let the sidebar's independent Shared read settle before measuring the picker-owned read.
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __pickerSharedReadCalls?: number })
            .__pickerSharedReadCalls ?? 0,
      ),
    )
    .toBeGreaterThanOrEqual(1);
  const sharedReadsBeforeOpen = await page.evaluate(() => {
    const target = window as unknown as {
      __pickerOrgListenerMode?: "hold";
      __pickerSharedReadCalls?: number;
    };
    target.__pickerOrgListenerMode = "hold";
    return target.__pickerSharedReadCalls ?? 0;
  });
  await openPicker(page);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __pickerOrgListenerHeld?: boolean })
            .__pickerOrgListenerHeld ?? false,
      ),
    )
    .toBe(true);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __pickerSharedReadCalls?: number })
          .__pickerSharedReadCalls ?? 0,
    ),
  ).toBe(sharedReadsBeforeOpen);
  await expect(picker(page).getByText("Shared Atlas", { exact: true })).toHaveCount(
    0,
  );

  await page.evaluate(() => {
    (
      window as unknown as { __releasePickerOrgListener: () => void }
    ).__releasePickerOrgListener();
  });
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __pickerSharedReadCalls?: number })
            .__pickerSharedReadCalls ?? 0,
      ),
    )
    .toBe(sharedReadsBeforeOpen + 1);
  await expect(
    picker(page).getByText("Shared Atlas", { exact: true }),
  ).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(picker(page)).toHaveCount(0);
  await page.evaluate(() => {
    (
      window as unknown as { __holdPickerSharedReply?: boolean }
    ).__holdPickerSharedReply = true;
  });
  await openPicker(page);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (
            window as unknown as { __resolvePickerSharedReply?: unknown }
          ).__resolvePickerSharedReply,
      ),
    )
    .toBe("function");
  await emit(page, "murmur://org-feed-updated");
  await expect(picker(page)).toHaveCount(0);
  await page.evaluate(() => {
    (
      window as unknown as { __resolvePickerSharedReply: () => void }
    ).__resolvePickerSharedReply();
  });
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  await expect(picker(page)).toHaveCount(0);

  const unregistersBeforeDestroyRace = await page.evaluate(() =>
    (
      window as unknown as {
        __demoEventListenerUnregisterCount: (event: string) => number;
      }
    ).__demoEventListenerUnregisterCount("murmur://org-feed-updated"),
  );
  await page.evaluate(() => {
    const target = window as unknown as {
      __holdPickerSharedReply?: boolean;
      __pickerOrgListenerMode?: "hold";
    };
    target.__holdPickerSharedReply = false;
    target.__pickerOrgListenerMode = "hold";
  });
  await openPicker(page);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __pickerOrgListenerHeld?: boolean })
            .__pickerOrgListenerHeld ?? false,
      ),
    )
    .toBe(true);
  await picker(page)
    .getByRole("button", { name: "Close the related picker" })
    .click();
  await expect(picker(page)).toHaveCount(0);
  await page.evaluate(() => {
    (
      window as unknown as { __releasePickerOrgListener: () => void }
    ).__releasePickerOrgListener();
  });
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __demoEventListenerUnregisterCount: (event: string) => number;
            }
          ).__demoEventListenerUnregisterCount("murmur://org-feed-updated"),
      ),
    )
    .toBeGreaterThan(unregistersBeforeDestroyRace);
});

test("keeps local hierarchy usable but never reads Shared when org-feed listener setup fails", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => [],
    get_backlinks: () => [],
    list_shared_workspace: () => {
      const target = window as unknown as {
        __pickerSharedFixture: unknown;
        __pickerSharedReadCalls?: number;
      };
      target.__pickerSharedReadCalls =
        (target.__pickerSharedReadCalls ?? 0) + 1;
      return target.__pickerSharedFixture;
    },
  });
  await installPickerOrgListenerGate(page);
  await page.addInitScript((fixture) => {
    (
      window as unknown as { __pickerSharedFixture: unknown }
    ).__pickerSharedFixture = fixture;
  }, sharedWorkspaceFixture());

  await page.goto("/notes/n1");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __pickerSharedReadCalls?: number })
            .__pickerSharedReadCalls ?? 0,
      ),
    )
    .toBeGreaterThanOrEqual(1);
  const sharedReadsBeforeOpen = await page.evaluate(() => {
    const target = window as unknown as {
      __pickerOrgListenerMode?: "reject";
      __pickerSharedReadCalls?: number;
    };
    target.__pickerOrgListenerMode = "reject";
    return target.__pickerSharedReadCalls ?? 0;
  });
  await openPicker(page);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __pickerOrgListenerRejected?: boolean })
            .__pickerOrgListenerRejected ?? false,
      ),
    )
    .toBe(true);
  await expect(
    picker(page).getByText("Workspace", { exact: true }),
  ).toBeVisible();
  await expect(
    picker(page).getByText("My First Note", { exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __pickerSharedReadCalls?: number })
          .__pickerSharedReadCalls ?? 0,
    ),
  ).toBe(sharedReadsBeforeOpen);
  await expect(picker(page).getByText("Shared Atlas", { exact: true })).toHaveCount(
    0,
  );
});

test("privacy invalidation closes and scrubs a pending search, while the modal stays inside short and narrow viewports", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => [],
    get_backlinks: () => [],
    search_related_picker: () => {
      const w = window as unknown as { __resolveLatePickerSearch?: () => void };
      return new Promise((resolve) => {
        w.__resolveLatePickerSearch = () =>
          resolve({
            offset: 0,
            hits: [
              {
                kind: "note",
                id: "secret-late",
                title: "Must never repaint",
                breadcrumb: ["Private"],
              },
            ],
            total: 1,
          });
      });
    },
  });

  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/notes/n1");
  await openPicker(page);
  for (const viewport of [
    { width: 1440, height: 900 },
    { width: 900, height: 680 },
    { width: 1280, height: 480 },
  ]) {
    await page.setViewportSize(viewport);
    const box = await picker(page).boundingBox();
    expect(box).not.toBeNull();
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.y).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(viewport.width);
    expect(box!.y + box!.height).toBeLessThanOrEqual(viewport.height);
  }
  await expect(picker(page)).toHaveCSS("overflow", "hidden");
  await expect(picker(page).locator(".rhp-tree")).toHaveCSS(
    "overflow-y",
    "auto",
  );

  await picker(page).getByPlaceholder("Search every Space…").fill("secret");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (window as unknown as { __resolveLatePickerSearch?: unknown })
            .__resolveLatePickerSearch,
      ),
    )
    .toBe("function");
  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://ask-history-invalidated", null);
  });
  await expect(picker(page)).toHaveCount(0);
  await page.evaluate(() => {
    (
      window as unknown as { __resolveLatePickerSearch: () => void }
    ).__resolveLatePickerSearch();
  });
  await expect(
    page.getByText("Must never repaint", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page
      .locator("app-connections")
      .getByRole("button", { name: "Link", exact: true }),
  ).toBeFocused();
});

test("Connections waits for its org-feed listener before reading relationships, then refetches locally", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => {
      const target = window as unknown as { __connectionsLinkReads?: number };
      target.__connectionsLinkReads =
        (target.__connectionsLinkReads ?? 0) + 1;
      return [
        {
          id: 951,
          direction: "out",
          otherKind: "note",
          otherId: "local-after-listener",
          otherTitle: "Local relation after listener",
          edgeType: "manual",
          createdBy: "user",
          status: "active",
          score: 1,
          createdAt: 1_720_000_000,
          manual: true,
        },
      ];
    },
    get_backlinks: () => {
      const target = window as unknown as {
        __connectionsBacklinkReads?: number;
      };
      target.__connectionsBacklinkReads =
        (target.__connectionsBacklinkReads ?? 0) + 1;
      return [];
    },
  });
  await installPickerOrgListenerGate(page, "hold");

  await page.goto("/notes/n1");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __pickerOrgListenerHeld?: boolean })
            .__pickerOrgListenerHeld ?? false,
      ),
    )
    .toBe(true);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __connectionsLinkReads?: number })
          .__connectionsLinkReads ?? 0,
    ),
  ).toBe(0);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __connectionsBacklinkReads?: number })
          .__connectionsBacklinkReads ?? 0,
    ),
  ).toBe(0);
  await expect(
    page.getByText("Local relation after listener", { exact: true }),
  ).toHaveCount(0);

  await page.evaluate(() => {
    (
      window as unknown as { __releasePickerOrgListener: () => void }
    ).__releasePickerOrgListener();
  });
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __connectionsLinkReads?: number })
            .__connectionsLinkReads ?? 0,
      ),
    )
    .toBeGreaterThanOrEqual(1);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __connectionsBacklinkReads?: number })
            .__connectionsBacklinkReads ?? 0,
      ),
    )
    .toBeGreaterThanOrEqual(1);
  const panel = page.locator("app-connections");
  await panel
    .getByRole("button", { name: "Show related items and suggestions" })
    .click();
  await expect(
    panel.getByText("Local relation after listener", { exact: true }),
  ).toBeVisible();
});

test("Connections fails closed without relationship reads when its org-feed listener is rejected", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => {
      const target = window as unknown as { __connectionsLinkReads?: number };
      target.__connectionsLinkReads =
        (target.__connectionsLinkReads ?? 0) + 1;
      return [
        {
          id: 952,
          direction: "out",
          otherKind: "org",
          otherId: "org-a:revoked",
          otherTitle: "Shared neighbour without listener",
          edgeType: "manual",
          createdBy: "user",
          status: "active",
          score: 1,
          createdAt: 1_720_000_001,
          manual: true,
        },
      ];
    },
    get_backlinks: () => {
      const target = window as unknown as {
        __connectionsBacklinkReads?: number;
      };
      target.__connectionsBacklinkReads =
        (target.__connectionsBacklinkReads ?? 0) + 1;
      return [
        {
          kind: "note",
          id: "revoked-backlink",
          title: "Backlink without listener",
        },
      ];
    },
  });
  await installPickerOrgListenerGate(page, "reject");

  await page.goto("/notes/n1");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __pickerOrgListenerRejected?: boolean })
            .__pickerOrgListenerRejected ?? false,
      ),
    )
    .toBe(true);
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __connectionsLinkReads?: number })
          .__connectionsLinkReads ?? 0,
    ),
  ).toBe(0);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __connectionsBacklinkReads?: number })
          .__connectionsBacklinkReads ?? 0,
    ),
  ).toBe(0);
  await expect(
    page.getByText("Shared neighbour without listener", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("Backlink without listener", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page
      .locator("app-connections")
      .getByRole("button", { name: "Link", exact: true }),
  ).toBeVisible();
});

test("org-feed callback synchronously invalidates Related before a stale reply can resolve", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => {
      const w = window as unknown as {
        __holdReplacementLinks?: boolean;
        __resolveReplacementLinks?: () => void;
      };
      const oldEdge = {
        id: 901,
        direction: "out",
        otherKind: "note",
        otherId: "sealed-neighbour",
        otherTitle: "Neighbour title that was revoked",
        edgeType: "manual",
        createdBy: "user",
        status: "active",
        score: 1,
        createdAt: 1_720_000_000,
        manual: true,
      };
      if (!w.__holdReplacementLinks) {
        return [oldEdge];
      }
      return new Promise((resolve) => {
        w.__resolveReplacementLinks = () => resolve([oldEdge]);
      });
    },
    get_backlinks: () => [],
    unlink_items: () => null,
  });

  await page.goto("/notes/n1");
  const panel = page.locator("app-connections");
  await panel
    .getByRole("button", { name: "Show related items and suggestions" })
    .click();
  await expect(
    panel.getByText("Neighbour title that was revoked", { exact: true }),
  ).toBeVisible();

  await page.evaluate(() => {
    (
      window as unknown as { __holdReplacementLinks?: boolean }
    ).__holdReplacementLinks = true;
  });
  await panel
    .getByRole("button", {
      name: "Remove link to Neighbour title that was revoked",
    })
    .click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (window as unknown as { __resolveReplacementLinks?: unknown })
            .__resolveReplacementLinks,
      ),
    )
    .toBe("function");
  await expect(
    panel.getByText("Neighbour title that was revoked", { exact: true }),
  ).toBeVisible();

  const visibleEdgesImmediatelyAfterEvent = await page.evaluate(() => {
    const target = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
      __resolveReplacementLinks: () => void;
      ng: {
        getComponent: (element: Element) => {
          edges: () => unknown[];
        };
      };
    };
    target.__demoEmit("murmur://org-feed-updated", null);
    const host = document.querySelector("app-connections");
    if (!host) {
      throw new Error("Connections host missing");
    }
    const visibleEdges = target.ng.getComponent(host).edges().length;
    // Resolve in the SAME JavaScript turn, before Angular's effect can run. Only the listener
    // callback's synchronous invalidation can stale this response in time.
    target.__resolveReplacementLinks();
    return visibleEdges;
  });
  expect(visibleEdgesImmediatelyAfterEvent).toBe(0);
  // Let the already-resolved IPC promise and Angular's zoneless render queue
  // drain. The stale reply must neither repaint nor survive the replacement effect.
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  await expect(
    panel.getByText("Neighbour title that was revoked", { exact: true }),
  ).toHaveCount(0);
});
