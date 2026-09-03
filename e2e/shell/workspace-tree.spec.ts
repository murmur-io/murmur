import { expect, test, type Page, type TestInfo } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * The workspace hierarchy in the contextual sidebar: Workspaces › Folders ›
 * mixed content rows.
 *
 * The forest below is written as the BACKEND serializes it (`ContainerNode` /
 * `TypeGroup` / `ItemRow` carry `rename_all = "camelCase"`), not as the
 * component would find convenient. A hand-written mock defines a shape; it does
 * not verify one — so the only thing that makes this fixture meaningful is that
 * every key here was copied from `src-tauri/src/storage/models.rs`. The
 * serialized-key oracle on the Rust side is what proves the backend agrees.
 */
const FOREST = [
  {
    id: "p-acme",
    name: "Acme",
    kind: "meeting",
    level: "project",
    emoji: "🟣",
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [
      {
        id: "f-q3",
        name: "Q3",
        kind: "note",
        level: "folder",
        emoji: null,
        tint: null,
        locked: false,
        unlocked: false,
        isRoot: false,
        folders: [],
        groups: [
          {
            kind: "note",
            total: 1,
            items: [
              {
                kind: "note",
                id: "n-plan",
                title: "Plan Q3",
                durationS: null,
                sortAt: 3,
              },
            ],
          },
        ],
      },
    ],
    groups: [
      {
        kind: "meeting",
        total: 5,
        items: [
          {
            kind: "meeting",
            id: "m-standup",
            title: "Standup",
            durationS: 900,
            sortAt: 90,
          },
          {
            kind: "meeting",
            id: "m-retro",
            title: null,
            durationS: 1800,
            sortAt: 40,
          },
          {
            kind: "meeting",
            id: "m-old",
            title: "Old sync",
            durationS: 600,
            sortAt: 10,
          },
        ],
      },
      {
        kind: "note",
        total: 3,
        items: [
          {
            kind: "note",
            id: "n-brief",
            title: "Launch brief",
            durationS: null,
            sortAt: 100,
          },
          {
            kind: "note",
            id: "n-risks",
            title: "Risks",
            durationS: null,
            sortAt: 60,
          },
        ],
      },
      {
        kind: "task",
        total: 2,
        items: [
          {
            kind: "task",
            id: "t-ship",
            title: "Ship release",
            durationS: null,
            sortAt: 80,
          },
          {
            kind: "task",
            id: "t-copy",
            title: "Review copy",
            durationS: null,
            sortAt: 30,
          },
        ],
      },
      {
        kind: "dashboard",
        total: 2,
        items: [
          {
            kind: "dashboard",
            id: "d-release",
            title: "Release dashboard",
            durationS: null,
            sortAt: 70,
          },
          {
            kind: "dashboard",
            id: "d-metrics",
            title: "Metrics",
            durationS: null,
            sortAt: 50,
          },
        ],
      },
    ],
  },
  {
    id: "p-private",
    name: "Private",
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    // Sealed and NOT session-unlocked: the backend sends no groups at all, not
    // even totals.
    locked: true,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
];

const AUDIT_FOREST = [
  FOREST[0],
  ...Array.from({ length: 18 }, (_, index) => ({
    id: `p-audit-${index + 1}`,
    name: `Audit Workspace ${index + 1}`,
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  })),
];

async function expectMenuItemAtHitPoint(
  page: Page,
  itemName: string,
): Promise<void> {
  const item = page.getByRole("menuitem", { name: itemName });
  await expect(item).toBeVisible();
  await item.scrollIntoViewIfNeeded();
  await expect
    .poll(() =>
      item.evaluate((element) => {
        const box = element.getBoundingClientRect();
        const hit = document.elementFromPoint(
          box.left + box.width / 2,
          box.top + box.height / 2,
        );
        return hit === element || Boolean(hit && element.contains(hit));
      }),
    )
    .toBe(true);
}

async function openWorkspace(page: Page): Promise<void> {
  await mockTauri(page, {}, { list_workspace_tree: FOREST });
  await page.goto("/");
  await expect(
    page.getByRole("navigation", { name: "Primary navigation" }),
  ).toBeVisible();
}

async function captureAuditScreenshot(
  page: Page,
  testInfo: TestInfo,
  name: string,
): Promise<void> {
  const path = testInfo.outputPath(`${name}.png`);
  await page.screenshot({ path, fullPage: false });
  await testInfo.attach(name, { path, contentType: "image/png" });
}

test("audits the complete sidebar at tall and short viewports", async ({
  page,
}, testInfo) => {
  const runtimeErrors: string[] = [];
  page.on("pageerror", (error) => runtimeErrors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") {
      runtimeErrors.push(message.text());
    }
  });
  await mockTauri(
    page,
    {},
    {
      list_workspace_tree: AUDIT_FOREST,
      list_container_items: { kind: "meeting", items: [], total: 0 },
    },
  );

  for (const viewport of [
    { name: "tall", width: 1440, height: 1000 },
    { name: "short", width: 1280, height: 480 },
  ] as const) {
    await page.setViewportSize(viewport);
    await page.goto("/meeting/m-standup");

    const sidebar = page.getByRole("navigation", { name: "Primary navigation" });
    const body = sidebar.locator(".sb-scroll");
    const selected = sidebar.getByRole("treeitem", { name: "Standup" });
    await expect(sidebar).toBeVisible();
    await expect(selected).toHaveAttribute("aria-selected", "true");
    expect(
      await selected.evaluate((row) => getComputedStyle(row).backgroundColor),
    ).not.toBe("rgba(0, 0, 0, 0)");

    const hovered = sidebar.getByRole("treeitem", { name: "Launch brief" });
    const restingBackground = await hovered.evaluate(
      (row) => getComputedStyle(row).backgroundColor,
    );
    await hovered.hover();
    await expect
      .poll(() =>
        hovered.evaluate((row) => getComputedStyle(row).backgroundColor),
      )
      .not.toBe(restingBackground);
    await captureAuditScreenshot(
      page,
      testInfo,
      `workspace-sidebar-${viewport.name}-states`,
    );

    const dimensions = await body.evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
    }));
    expect(dimensions.scrollHeight).toBeGreaterThan(dimensions.clientHeight);

    const lastRow = sidebar.getByRole("treeitem", { name: /Audit Workspace 18/ });
    await lastRow.scrollIntoViewIfNeeded();
    await expect(lastRow).toBeVisible();
    await expect
      .poll(() =>
        body.evaluate((element) => ({
          atBottom:
            element.scrollTop + element.clientHeight >=
            element.scrollHeight - 1,
          scrolled: element.scrollTop > 0,
        })),
      )
      .toEqual({ atBottom: true, scrolled: true });

    const footer = sidebar.getByRole("button", {
      name: "Collapse sidebar",
    });
    await expect(footer).toBeVisible();
    await expect
      .poll(() =>
        footer.evaluate((element) => {
          const box = element.getBoundingClientRect();
          const hit = document.elementFromPoint(
            box.left + box.width / 2,
            box.top + box.height / 2,
          );
          return hit === element || Boolean(hit && element.contains(hit));
        }),
      )
      .toBe(true);

    const overflowTrigger = sidebar.getByRole("button", {
      name: "Actions for Audit Workspace 18",
    });
    await overflowTrigger.click();
    const overflowMenu = page.getByRole("menu", {
      name: "Actions for Audit Workspace 18",
    });
    await expect(overflowMenu).toBeVisible();
    await expect
      .poll(() =>
        overflowMenu.evaluate((menu) => ({
          parent: menu.parentElement?.tagName ?? null,
          withinBoundary: (() => {
            const panel = menu.getBoundingClientRect();
            const scroller = document
              .querySelector<HTMLElement>(".primary-sidebar .sb-scroll")!
              .getBoundingClientRect();
            return (
              panel.top >= scroller.top && panel.bottom <= scroller.bottom + 1
            );
          })(),
        })),
      )
      .toEqual({ parent: "BODY", withinBoundary: true });
    await captureAuditScreenshot(
      page,
      testInfo,
      `workspace-sidebar-${viewport.name}-overflow-menu`,
    );
    await expectMenuItemAtHitPoint(page, "Rename Workspace");
    await page.keyboard.press("Escape");
    await expect(overflowMenu).toHaveCount(0);
    await expect(overflowTrigger).toBeFocused();
  }

  expect(runtimeErrors).toEqual([]);
});

test("keeps the single contextual container menu above following tree rows", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1000, height: 480 });
  await openWorkspace(page);
  await page.getByRole("button", { name: "Expand Acme" }).click();

  const acmeTrigger = page.getByRole("button", { name: "Actions for Acme" });
  await acmeTrigger.focus();
  await page.keyboard.press("Enter");
  const acmeMenu = page.getByRole("menu", { name: "Actions for Acme" });
  await expect(acmeMenu).toBeVisible();
  expect(await acmeMenu.evaluate((menu) => menu.parentElement?.tagName)).toBe(
    "BODY",
  );
  await expect(
    page.getByRole("menuitem", { name: "Create note here" }),
  ).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(
    page.getByRole("menuitem", { name: "Create folder here" }),
  ).toBeFocused();
  await expectMenuItemAtHitPoint(page, "Create folder here");
  await expect(
    page
      .getByRole("menuitem", { name: "Create folder here" })
      .locator("mur-icon"),
  ).toHaveAttribute("data-icon", "folder-add");
  await expectMenuItemAtHitPoint(page, "Rename Workspace");
  await expect(page.getByText("Workspace actions", { exact: true })).toBeVisible();
  await expect(
    page.getByRole("menuitem", { name: "Rename Workspace" }).locator("mur-icon"),
  ).toHaveAttribute("data-icon", "rename");
  await expect(
    page.getByRole("menuitem", { name: "Delete Workspace" }).locator("mur-icon"),
  ).toHaveAttribute("data-icon", "trash");

  await page.keyboard.press("Escape");

  // The panel lives under BODY, so scrolling the sidebar no longer moves it via
  // normal layout. Open a non-boundary row and prove the shared capture-phase
  // reposition directive keeps the teleported surface anchored to its trigger
  // instead of leaving a stale menu floating over an unrelated row.
  const recordingTrigger = page.getByRole("button", {
    name: "Actions for meeting Standup",
  });
  await recordingTrigger.click();
  const recordingMenu = page.getByRole("menu", {
    name: "Actions for meeting Standup",
  });
  await expect(recordingMenu).toBeVisible();
  const gapBefore = await recordingMenu.evaluate((menu) => {
    const trigger = document.querySelector<HTMLElement>(
      "button[aria-label='Actions for meeting Standup']",
    );
    if (!trigger) return null;
    return Math.round(
      menu.getBoundingClientRect().top - trigger.getBoundingClientRect().bottom,
    );
  });
  expect(gapBefore).not.toBeNull();
  const contextBody = page.locator(".primary-sidebar .sb-scroll");
  await contextBody.evaluate((element) => element.scrollBy({ top: 16 }));
  await expect
    .poll(() => contextBody.evaluate((element) => element.scrollTop))
    .toBeGreaterThan(0);
  await expect
    .poll(() =>
      recordingMenu.evaluate((menu) => {
        const trigger = document.querySelector<HTMLElement>(
          "button[aria-label='Actions for meeting Standup']",
        );
        if (!trigger) return null;
        const menuBox = menu.getBoundingClientRect();
        const triggerBox = trigger.getBoundingClientRect();
        return {
          parent: menu.parentElement?.tagName ?? null,
          gap: Math.round(menuBox.top - triggerBox.bottom),
        };
      }),
    )
    .toEqual({ parent: "BODY", gap: gapBefore });
});

test("paints a row menu opaque on its first rendered frame", async ({
  page,
}) => {
  await openWorkspace(page);

  const trigger = page.getByRole("button", { name: "Actions for Acme" });
  const firstFrame = await trigger.evaluate(async (button) => {
    button.click();
    await Promise.resolve();
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => resolve()),
    );

    const host = button.closest<HTMLElement>("mur-row-menu");
    const panel = document.querySelector<HTMLElement>(
      "body > [role='menu'][aria-label='Actions for Acme']",
    );
    if (!host || !panel) {
      return null;
    }
    const background = getComputedStyle(panel).backgroundColor;
    const channels = background.match(/[\d.]+/g)?.map(Number) ?? [];
    return {
      hostOpacity: Number(getComputedStyle(host).opacity),
      panelOpacity: Number(getComputedStyle(panel).opacity),
      backgroundAlpha: channels.length === 4 ? channels[3] : 1,
      parent: panel.parentElement?.tagName ?? null,
    };
  });

  expect(firstFrame).toEqual({
    hostOpacity: 1,
    panelOpacity: 1,
    backgroundAlpha: 1,
    parent: "BODY",
  });
});

test("rename and delete use explicit contextual confirmation instead of native prompts", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      rename_folder: (args: unknown) => {
        const target = window as unknown as { __renames?: unknown[] };
        (target.__renames ??= []).push(args);
        return {
          id: "p-acme",
          name: "Acme Studio",
          path: "Acme Studio",
          parentId: null,
          noteCount: 0,
          locked: false,
          unlocked: false,
          kind: "meeting",
        };
      },
      delete_folder: (args: unknown) => {
        const target = window as unknown as { __deletes?: unknown[] };
        (target.__deletes ??= []).push(args);
        return null;
      },
    },
    { list_workspace_tree: FOREST },
  );
  await page.goto("/");

  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.getByRole("menuitem", { name: "Rename Workspace" }).click();
  const rename = page.getByRole("dialog", { name: "Rename Workspace Acme" });
  await expect(rename).toBeVisible();
  await rename.getByLabel("Name").fill("Acme Studio");
  await rename.getByRole("button", { name: "Rename", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __renames?: unknown[] }).__renames ?? [],
      ),
    )
    .toEqual([{ folderId: "p-acme", newName: "Acme Studio" }]);

  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.getByRole("menuitem", { name: "Delete Workspace" }).click();
  const remove = page.getByRole("dialog", { name: "Delete Workspace Acme" });
  await expect(remove).toContainText("Its items are kept and moved out");
  expect(
    await page.evaluate(
      () => (window as unknown as { __deletes?: unknown[] }).__deletes ?? [],
    ),
  ).toEqual([]);
  await remove.getByRole("button", { name: "Delete Workspace" }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __deletes?: unknown[] }).__deletes ?? [],
      ),
    )
    .toEqual([{ folderId: "p-acme" }]);
});

test("shows unfiled recordings as a real inbox and opens the complete meetings list", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      list_container_items: (args: {
        containerId: string | null;
        kind: string;
        offset: number;
        limit: number;
      }) => {
        if (
          args.containerId !== null ||
          args.kind !== "meeting" ||
          args.offset !== 0 ||
          args.limit !== 8
        ) {
          throw new Error("unexpected unfiled page request");
        }
        return {
          kind: "meeting",
          total: 12,
          items: Array.from({ length: 8 }, (_, index) => ({
            kind: "meeting",
            id: `m-unfiled-${12 - index}`,
            title: `Unfiled recording ${12 - index}`,
            durationS: 600 + index,
            sortAt: 120 - index,
          })),
        };
      },
    },
    { list_workspace_tree: FOREST },
  );
  await page.goto("/");

  const inbox = page.getByRole("treeitem", { name: /Unfiled recordings/ });
  await expect(inbox).toBeVisible();
  await expect(inbox).toContainText("12");
  await expect(inbox.getByRole("button", { name: /Add to/ })).toHaveCount(0);
  await expect(inbox.getByRole("button", { name: /Actions for/ })).toHaveCount(
    0,
  );
  await expect(inbox).not.toHaveAttribute("appfolderdrop");

  const tree = page.getByRole("tree", { name: "Workspaces" });
  const unfiledRows = tree.locator(".line--unfiled-item");
  await expect(unfiledRows).toHaveCount(8);
  await expect(unfiledRows).toHaveText([
    "Unfiled recording 12",
    "Unfiled recording 11",
    "Unfiled recording 10",
    "Unfiled recording 9",
    "Unfiled recording 8",
    "Unfiled recording 7",
    "Unfiled recording 6",
    "Unfiled recording 5",
  ]);
  const moveNewest = page.getByRole("button", {
    name: "Actions for recording Unfiled recording 12",
  });
  await expect(moveNewest).toBeVisible();
  await moveNewest.click();
  await page
    .getByRole("menuitem", { name: "Move to Workspace or folder…" })
    .click();
  await expect(
    page
      .getByRole("dialog", {
        name: "Move recording “Unfiled recording 12” to Workspace",
      })
      .getByRole("button", { name: "Move to Acme", exact: true }),
  ).toBeVisible();
  await page.keyboard.press("Escape");

  await page
    .getByRole("button", { name: "Collapse Unfiled recordings" })
    .click();
  await expect(unfiledRows).toHaveCount(0);
  expect(
    await page.evaluate(() =>
      localStorage.getItem("murmur.workspace.unfiledExpanded"),
    ),
  ).toBe("false");
  await page.reload();
  await expect(unfiledRows).toHaveCount(0);
  await page.getByRole("button", { name: "Expand Unfiled recordings" }).click();
  await expect(unfiledRows).toHaveCount(8);

  // Prove the destination clears an existing meeting-folder scope instead of
  // merely changing the URL while Library remains filtered to that Workspace.
  await page
    .getByRole("treeitem", { name: /Acme/ })
    .getByRole("button", { name: "Acme", exact: true })
    .click();
  await expect(page).toHaveURL(/\/container\/p-acme$/);

  await page
    .getByRole("treeitem", { name: "View all recordings (12)" })
    .click();
  await expect(page).toHaveURL(/\/library$/);
  await expect(
    page.getByRole("heading", { name: "Meetings", exact: true }),
  ).toBeVisible();
});

test("scrubs unfiled titles synchronously and drops a late pre-invalidation page", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      list_container_items: () => {
        const target = window as unknown as {
          __unfiledCalls?: number;
          __releaseLateUnfiled?: () => void;
        };
        target.__unfiledCalls = (target.__unfiledCalls ?? 0) + 1;
        if (target.__unfiledCalls === 1) {
          return {
            kind: "meeting",
            total: 1,
            items: [
              {
                kind: "meeting",
                id: "m-mounted-secret",
                title: "Mounted private recording",
                durationS: 300,
                sortAt: 10,
              },
            ],
          };
        }
        if (target.__unfiledCalls === 2) {
          return new Promise((resolve) => {
            target.__releaseLateUnfiled = () =>
              resolve({
                kind: "meeting",
                total: 1,
                items: [
                  {
                    kind: "meeting",
                    id: "m-late-secret",
                    title: "Late private recording",
                    durationS: 300,
                    sortAt: 20,
                  },
                ],
              });
          });
        }
        // The repair read must not hide a stale-response bug by winning the
        // race and replacing call #2 with a safe empty page. A real privacy
        // transition may be followed by an unavailable reader, so the
        // generation guard itself has to keep the late secret from landing.
        return Promise.reject(
          new Error("post-invalidation refresh unavailable"),
        );
      },
    },
    { list_workspace_tree: FOREST },
  );
  await page.goto("/");
  await expect(
    page.getByText("Mounted private recording", { exact: true }),
  ).toBeVisible();

  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://reminder-visibility-invalidated", null);
  });
  await expect(
    page.getByText("Mounted private recording", { exact: true }),
  ).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __unfiledCalls?: number }).__unfiledCalls ??
          0,
      ),
    )
    .toBe(2);

  await page.evaluate(() => {
    const target = window as unknown as { __releaseLateUnfiled?: () => void };
    target.__releaseLateUnfiled?.();
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://reminder-visibility-invalidated", null);
  });
  await expect(
    page.getByText("Late private recording", { exact: true }),
  ).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __unfiledCalls?: number }).__unfiledCalls ??
          0,
      ),
    )
    .toBe(3);
});

test("renders one flat mixed stream below each expanded container", async ({
  page,
}) => {
  await openWorkspace(page);

  const tree = page.getByRole("tree", { name: "Workspaces" });
  await expect(tree).toBeVisible();

  await expect(page.getByRole("treeitem", { name: /Acme/ })).toBeVisible();
  await expect(page.getByRole("treeitem", { name: /Private/ })).toBeVisible();

  // A selected Workspace can still be collapsed by the user.
  await expect(
    page.getByRole("treeitem", { name: /Launch brief/ }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Expand Acme" }).click();

  // No synthetic kind headers: direct children are globally newest-first.
  await expect(tree.locator(".line--group")).toHaveCount(0);
  const mixedRows = tree.locator(".line--item");
  await expect(mixedRows).toHaveCount(8);
  await expect(mixedRows).toHaveText([
    "Launch brief",
    "Standup",
    "Ship release",
    "Release dashboard",
    "Risks",
    "Metrics",
    "Untitled",
    "Review copy",
  ]);
  for (const row of await mixedRows.all()) {
    await expect(row).toHaveAttribute("aria-level", "2");
  }

  // An untitled item renders a placeholder rather than an empty row.
  await expect(page.getByRole("treeitem", { name: /Untitled/ })).toBeVisible();

  // All kinds share one total and one continuation row.
  await expect(
    page.getByRole("treeitem", { name: /View all \(12\)/ }),
  ).toHaveCount(1);

  // A child folder is rendered under its project, with its own groups.
  await expect(page.getByRole("treeitem", { name: /Q3/ })).toBeVisible();
});

test("renders distinct, type-colored Workspace and content glyphs without washing out selection", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      list_workspace_tree: [{ ...FOREST[0], emoji: null }, FOREST[1]],
    },
  );
  await page.goto("/meeting/m-standup");

  const spaceIcon = page
    .getByRole("treeitem", { name: /Acme/ })
    .locator(".row-icon");
  const folderIcon = page
    .getByRole("treeitem", { name: /Q3/ })
    .locator(".row-icon");
  const selectedMeeting = page.getByRole("treeitem", { name: "Standup" });
  const selectedMeetingIcon = selectedMeeting.locator(".row-icon");
  const unselectedMeetingIcon = page
    .getByRole("treeitem", { name: "Untitled" })
    .locator(".row-icon");
  const noteIcon = page
    .getByRole("treeitem", { name: "Launch brief" })
    .locator(".row-icon");
  const taskIcon = page
    .getByRole("treeitem", { name: "Ship release" })
    .locator(".row-icon");
  const dashboardIcon = page
    .getByRole("treeitem", { name: "Release dashboard" })
    .locator(".row-icon");
  const lockedIcon = page
    .getByRole("treeitem", { name: /Private/ })
    .locator(".row-icon");

  await expect(spaceIcon).toHaveAttribute("data-icon", "space");
  await expect(folderIcon).toHaveAttribute("data-icon", "folder");
  await expect(spaceIcon.locator("svg rect")).toHaveCount(4);
  await expect(folderIcon.locator("svg path")).toHaveCount(1);
  await expect(lockedIcon).toHaveAttribute("data-icon", "locked");

  const typeIcons = [
    spaceIcon,
    folderIcon,
    selectedMeetingIcon,
    noteIcon,
    taskIcon,
    dashboardIcon,
    lockedIcon,
  ];
  const typeColors = await Promise.all(
    typeIcons.map((icon) =>
      icon.evaluate((element) => getComputedStyle(element).color),
    ),
  );
  expect(new Set(typeColors).size).toBe(typeColors.length);

  await expect(selectedMeeting).toHaveAttribute("aria-selected", "true");
  await expect(unselectedMeetingIcon).toHaveAttribute("data-icon", "meeting");
  expect(typeColors[2]).toBe(
    await unselectedMeetingIcon.evaluate(
      (element) => getComputedStyle(element).color,
    ),
  );
});

test("keeps an older selected leaf within the eight-row cap", async ({
  page,
}) => {
  await mockTauri(page, {}, { list_workspace_tree: FOREST });
  await page.goto("/meeting/m-old");

  const tree = page.getByRole("tree", { name: "Workspaces" });
  await expect(tree).toBeVisible();
  const mixedRows = tree.locator(".line--item");
  await expect(mixedRows).toHaveCount(8);
  await expect(mixedRows).toHaveText([
    "Launch brief",
    "Standup",
    "Ship release",
    "Release dashboard",
    "Risks",
    "Metrics",
    "Untitled",
    "Old sync",
  ]);
  await expect(
    page.getByRole("treeitem", { name: "Old sync" }),
  ).toHaveAttribute("aria-selected", "true");
  await expect(
    page.getByRole("treeitem", { name: /View all \(12\)/ }),
  ).toHaveCount(1);
});

test("a sealed project discloses nothing about what it holds", async ({
  page,
}) => {
  await openWorkspace(page);

  const sealed = page.getByRole("treeitem", { name: /Private/ });
  await expect(sealed).toBeVisible();

  // No counts: the backend refused to describe the contents, so the tree must
  // not imply it knows them — and "0" would be a claim, not an absence.
  await expect(sealed).not.toContainText(/\d/);

  // And no disclosure control, because there is nothing to disclose. Offering
  // one that expands to emptiness would read as "this project is empty".
  await expect(
    page.getByRole("button", { name: "Expand Private" }),
  ).toHaveCount(0);
});

test("treats a sealed container as an intrinsic leaf even when a stale payload includes content", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      list_workspace_tree: [
        {
          id: "p-sealed-stale",
          name: "Private",
          kind: "meeting",
          level: "project",
          emoji: "🔒",
          tint: "violet",
          locked: true,
          unlocked: false,
          isRoot: false,
          folders: [
            {
              id: "f-secret",
              name: "Secret child",
              kind: "meeting",
              level: "folder",
              emoji: null,
              tint: null,
              locked: true,
              unlocked: false,
              isRoot: false,
              folders: [],
              groups: [],
            },
          ],
          groups: [
            {
              kind: "note",
              total: 1,
              items: [
                {
                  kind: "note",
                  id: "n-secret",
                  title: "Secret launch title",
                  durationS: null,
                  sortAt: 1,
                },
              ],
            },
          ],
        },
      ],
    },
  );
  await page.goto("/container/p-sealed-stale");

  const sealed = page.getByRole("treeitem", { name: "Private" });
  await expect(sealed).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Expand Private" }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Actions for Private" }),
  ).toHaveCount(1);
  await page.getByRole("button", { name: "Actions for Private" }).click();
  await expect(
    page.getByRole("menuitem", { name: "Unlock for this session" }),
  ).toBeVisible();
  await expect(page.getByRole("menuitem")).toHaveCount(1);
  await expect(page.getByText("Secret child", { exact: true })).toHaveCount(0);
  await expect(
    page.getByText("Secret launch title", { exact: true }),
  ).toHaveCount(0);
});

test("every rendered Workspace, folder and content row exposes exactly one ellipsis menu", async ({
  page,
}) => {
  await openWorkspace(page);
  await page.getByRole("button", { name: "Expand Acme" }).click();
  await page.getByRole("button", { name: "Expand Q3" }).click();

  const namedRows = [
    /Acme/,
    /Q3/,
    /Launch brief/,
    /Standup/,
    /Ship release/,
    /Release dashboard/,
    /Plan Q3/,
  ];
  for (const name of namedRows) {
    const row = page.getByRole("treeitem", { name }).first();
    await expect(row.locator(".row-menu-trigger")).toHaveCount(1);
    await expect(row.locator(".row-menu-trigger svg circle")).toHaveCount(3);
  }
  await expect(
    page.locator(".row-move-trigger, mur-row-menu.row-add"),
  ).toHaveCount(0);
});

test("scrubs cached hierarchy titles when relock succeeds even if every refresh rejects", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      list_workspace_tree: () => {
        const target = window as unknown as { __workspaceTreeCalls?: number };
        target.__workspaceTreeCalls = (target.__workspaceTreeCalls ?? 0) + 1;
        if (target.__workspaceTreeCalls > 1) {
          return Promise.reject(new Error("workspace refresh unavailable"));
        }
        return [
          {
            id: "p-private",
            name: "Private",
            kind: "meeting",
            level: "project",
            emoji: null,
            tint: null,
            locked: true,
            unlocked: true,
            isRoot: false,
            folders: [],
            groups: [
              {
                kind: "note",
                total: 1,
                items: [
                  {
                    kind: "note",
                    id: "n-secret",
                    title: "Acquisition codename",
                    durationS: null,
                    sortAt: 1,
                  },
                ],
              },
              {
                kind: "meeting",
                total: 1,
                items: [
                  {
                    kind: "meeting",
                    id: "m-secret",
                    title: "Board compensation",
                    durationS: 600,
                    sortAt: 2,
                  },
                ],
              },
            ],
          },
        ];
      },
      relock_all: () => {
        (
          window as unknown as {
            __demoEmit: (event: string, payload: unknown) => void;
          }
        ).__demoEmit("murmur://reminder-visibility-invalidated", null);
        return null;
      },
    },
    {
      list_folders: [
        {
          id: "p-private",
          name: "Private",
          path: "Private",
          parentId: null,
          noteCount: 2,
          locked: true,
          unlocked: true,
          kind: "meeting",
          children: [],
        },
      ],
    },
  );
  await page.goto("/meeting/m-secret");

  await expect(
    page.getByText("Board compensation", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Acquisition codename", { exact: true }),
  ).toBeVisible();

  await page
    .getByRole("button", { name: "Re-seal all 1 unlocked folder now" })
    .click();

  await expect(
    page.getByText("Board compensation", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("Acquisition codename", { exact: true }),
  ).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __workspaceTreeCalls?: number })
            .__workspaceTreeCalls ?? 0,
      ),
    )
    .toBeGreaterThan(1);
  await expect(
    page.getByText("Board compensation", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("Acquisition codename", { exact: true }),
  ).toHaveCount(0);
});
