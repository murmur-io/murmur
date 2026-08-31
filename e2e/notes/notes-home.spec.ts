import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Notes home is an exact-list route: the one sidebar owns navigation, with the
 * Browse disclosure holding the list destinations. The
 * content pane still renders the note table, including the sealed row, with
 * no console/page errors — the runtime check that catches NG0600 / ɵcmp /
 * forwardRef regressions a green `ng build` misses.
 */
test("notes home renders Browse navigation + the note table (incl. masked locked row) with no console errors", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page);
  await page.goto("/notes");

  // The content pane proves the route resolved.
  await expect(page.locator(".notes-content")).toBeVisible();

  const globalNavigation = page.getByRole("navigation", {
    name: "Primary navigation",
  });
  await expect(globalNavigation).toBeVisible();

  // Browse is a disclosure group inside the one sidebar (it used to be its own
  // "Browse sidebar" complementary panel) and it starts collapsed, so the
  // active-destination assertion has to open it first.
  const browseSidebar = page.getByRole("navigation", {
    name: "Browse destinations",
  });
  await expect(browseSidebar).toBeVisible();
  await browseSidebar
    .getByRole("button", { name: "Browse", exact: true })
    .click();
  await expect(
    browseSidebar.getByRole("link", { name: "Notes", exact: true }),
  ).toHaveClass(/active/);

  // The prominent "New note" action.
  await expect(page.locator(".new-note-btn")).toBeVisible();

  // There is exactly ONE sidebar on every route now — the hierarchy is no longer
  // a second panel that some routes mount and others do not. Asserting the OLD
  // panel is absent would pass vacuously (its class no longer exists), so assert
  // the real invariant instead.
  await expect(page.locator("mur-sidebar.primary-sidebar")).toHaveCount(1);
  await expect(page.locator("mur-sidebar")).toHaveCount(1);
  await expect(page.locator("mur-sidebar-section")).toHaveCount(0);

  // The table renders (thead + the visible note row + its tag pill).
  await expect(
    page.locator(".mur-table thead th", { hasText: "Title" }),
  ).toBeVisible();
  await expect(page.getByText("My First Note")).toBeVisible();
  await expect(page.locator(".note-tag", { hasText: "idea" })).toBeVisible();

  // The masked (sealed-not-unlocked) note row shows the lock title, no snippet.
  await expect(page.getByText("🔒 Locked")).toBeVisible();

  // No NG0600 / ɵcmp / any other console error surfaced through the render.
  expect(consoleErrors).toEqual([]);
});

test("Notes Home hides auto-organize for a session-unlocked sealed folder", async ({
  page,
}) => {
  await mockNotes(page, {
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
            id: "nf2",
            name: "Work",
            level: "folder",
            emoji: null,
            tint: null,
            locked: true,
            unlocked: true,
            isRoot: false,
            folders: [],
            groups: [],
          },
        ],
        groups: [],
      },
    ],
    list_note_folders: () => [
      {
        id: "nf2",
        name: "Work",
        path: "Notes/Work",
        parentId: null,
        locked: true,
        unlocked: true,
        isRoot: false,
        kind: "note",
      },
    ],
    get_container: () => ({
      id: "nf2",
      name: "Work",
      level: "folder",
      emoji: null,
      tint: null,
      locked: true,
      unlocked: true,
      isRoot: false,
      folders: [],
      groups: [],
    }),
  });
  await page.goto("/notes");
  // The Workspaces tree is a section of the ONE sidebar now, rather than a
  // separate "Workspaces sidebar" panel opened from a rail button.
  const spacesSidebar = page.getByRole("navigation", {
    name: "Primary navigation",
  });
  await spacesSidebar.getByRole("button", { name: "Expand Workspace" }).click();
  await spacesSidebar
    .getByRole("button", { name: "Work", exact: true })
    .click();
  await expect(page).toHaveURL(/\/container\/nf2$/);

  await page
    .getByRole("navigation", { name: "Primary navigation" })
    .getByRole("button", { name: "Browse", exact: true })
    .click();
  await page
    .getByRole("navigation", { name: "Browse destinations" })
    .getByRole("link", { name: "Notes", exact: true })
    .click();
  await expect(page).toHaveURL(/\/notes$/);
  await expect(page.locator(".content-title")).toHaveText("Work");
  await expect(page.locator(".organize-btn")).toHaveCount(0);
});

test("workspace organizer warns before de-sealing and excludes that move from Select all", async ({
  page,
}) => {
  const runtimeErrors: string[] = [];
  page.on("pageerror", (error) => runtimeErrors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") {
      runtimeErrors.push(message.text());
    }
  });

  await mockNotes(page, {
    list_workspace_tree: () => [
      {
        id: "sealed-space",
        name: "Private Workspace",
        kind: "meeting",
        level: "project",
        emoji: "🔒",
        tint: null,
        locked: true,
        unlocked: true,
        isRoot: false,
        folders: [],
        groups: [],
      },
      {
        id: "open-space",
        name: "Team Workspace",
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
    ],
    list_container_items: (args: {
      containerId: string | null;
      kind: string;
    }) =>
      args.containerId === null && args.kind === "meeting"
        ? {
            kind: "meeting",
            total: 2,
            items: [
              {
                kind: "meeting",
                id: "sealed-recording",
                title: "Private roadmap",
                durationS: 900,
                sortAt: 2,
              },
              {
                kind: "meeting",
                id: "open-recording",
                title: "Public standup",
                durationS: 600,
                sortAt: 1,
              },
            ],
          }
        : { kind: args.kind, total: 0, items: [] },
    plan_workspace_organization: () => ({
      moves: [
        {
          itemId: "sealed-recording",
          title: "Private roadmap",
          fromContainerId: "sealed-space",
          fromContainer: "Private Workspace",
          toContainerId: "open-space",
          toContainer: "Team Workspace",
          reason: "Matches the team's roadmap work",
        },
        {
          itemId: "open-recording",
          title: "Public standup",
          fromContainerId: null,
          fromContainer: "Unfiled",
          toContainerId: "open-space",
          toContainer: "Team Workspace",
          reason: "Recurring team sync",
        },
      ],
      review: [],
      skipped: [],
      targets: [{ id: "open-space", label: "Team Workspace" }],
      totalScanned: 2,
    }),
  });
  await page.goto("/notes");
  await page
    .getByRole("button", { name: "Review filing moves with Brain" })
    .click();

  const sheet = page.getByRole("dialog", {
    name: "Review Brain filing plan",
  });
  const privateMove = sheet.getByRole("checkbox", {
    name: "Move Private roadmap to Team Workspace",
  });
  const safeMove = sheet.getByRole("checkbox", {
    name: "Move Public standup to Team Workspace",
  });
  const warning = sheet.getByTestId("organizer-unsealed-warning");

  await expect(warning).toContainText("Privacy change");
  await expect(
    warning.getByRole("img", { name: "Unlocked for this session" }),
  ).toBeVisible();
  await expect(warning).toContainText(
    "Moving from this session-unlocked source to an open destination will store the recording unsealed.",
  );
  await expect(privateMove).not.toBeChecked();
  await expect(safeMove).toBeChecked();
  await expect(sheet.getByText("1 selected to move")).toBeVisible();

  await sheet.getByRole("button", { name: "Clear", exact: true }).click();
  await expect(privateMove).not.toBeChecked();
  await expect(safeMove).not.toBeChecked();
  await sheet.getByRole("button", { name: "Select all", exact: true }).click();
  await expect(privateMove).not.toBeChecked();
  await expect(safeMove).toBeChecked();

  // The privacy-changing row remains available for an explicit, individual opt-in.
  await privateMove.check();
  await expect(privateMove).toBeChecked();
  await expect(sheet.getByText("2 selected to move")).toBeVisible();
  expect(runtimeErrors).toEqual([]);
});

/**
 * Auto-organize drives end-to-end: the header button fetches a plan, the review
 * sheet opens (opaque T3), Apply calls `apply_organize_plan` and closes — all
 * with no console errors.
 */
test("auto-organize applies the reviewed scope without frontend receipt fields", async ({
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
    apply_organize_plan: (args: unknown) => {
      const target = globalThis as unknown as {
        __organizeApplyCalls?: unknown[];
      };
      (target.__organizeApplyCalls ??= []).push(args);
      return { appliedIds: ["n1"], failures: [] };
    },
  });
  await page.goto("/notes");
  await expect(page.locator(".notes-content")).toBeVisible();

  // Open the review sheet.
  await page.locator(".organize-btn").click();
  const sheet = page.locator("app-organize-sheet .sheet");
  await expect(sheet).toBeVisible();
  // The proposed move: "My First Note" → "Ideas" (a NEW folder).
  await expect(sheet.getByText("My First Note", { exact: true })).toBeVisible();
  await expect(sheet.locator(".move-row select")).toHaveValue("");
  await expect(
    sheet.locator(".move-row option", { hasText: "New folder: Ideas" }),
  ).toHaveCount(1);

  // Apply → the sheet closes (apply_organize_plan resolved).
  await sheet.locator(".sheet-actions .btn-primary").click();
  await expect(sheet).toBeHidden();

  const applyCalls = await page.evaluate(
    () =>
      (globalThis as unknown as { __organizeApplyCalls?: unknown[] })
        .__organizeApplyCalls ?? [],
  );
  expect(applyCalls).toHaveLength(1);
  expect(applyCalls[0]).toMatchObject({
    plan: {
      scopeFolderId: null,
      totalScanned: 3,
      alreadyOrganized: 1,
      deferred: 1,
      targets: [],
      moves: [{ noteId: "n1", toFolder: "Ideas" }],
    },
  });
  const wirePlan = (applyCalls[0] as { plan: Record<string, unknown> }).plan;
  expect(wirePlan).not.toHaveProperty("receipt");
  expect(wirePlan).not.toHaveProperty("applyError");
  expect(
    (wirePlan["moves"] as Record<string, unknown>[])[0],
  ).not.toHaveProperty("reviewScopeFolderId");

  expect(consoleErrors).toEqual([]);
});

test("Notes Home replans with custom guidance and replaces stale local review state", async ({
  page,
}) => {
  await mockNotes(page, {
    plan_organize_notes: (args: unknown) => {
      const target = globalThis as unknown as {
        __organizePlanCalls?: unknown[];
      };
      const calls = (target.__organizePlanCalls ??= []);
      calls.push(args);
      if (calls.length === 1) {
        return {
          scopeFolderId: null,
          moves: [
            {
              noteId: "n1",
              title: "Old suggestion",
              fromFolderId: "nf1",
              fromFolder: "Notes",
              toFolder: "Ideas",
              toFolderId: "nf-ideas",
              reason: "The original model suggestion",
              confidence: "low",
            },
          ],
          totalScanned: 1,
          alreadyOrganized: 0,
          deferred: 0,
          targets: [
            { id: "nf-ideas", label: "Notes / Ideas" },
            { id: "nf-alternate", label: "Notes / Alternate" },
          ],
        };
      }
      return {
        scopeFolderId: null,
        moves: [
          {
            noteId: "n2",
            title: "Fresh guided suggestion",
            fromFolderId: "nf1",
            fromFolder: "Notes",
            toFolder: "Weekly",
            toFolderId: "nf-weekly",
            reason: "The custom guidance prefers weekly notes",
            confidence: "high",
          },
        ],
        totalScanned: 2,
        alreadyOrganized: 1,
        deferred: 0,
        targets: [{ id: "nf-weekly", label: "Notes / Weekly" }],
      };
    },
  });
  await page.goto("/notes");
  await expect(page.locator(".notes-content")).toBeVisible();

  await page.locator(".organize-btn").click();
  const sheet = page.locator("app-organize-sheet .sheet");
  await expect(
    sheet.getByText("Old suggestion", { exact: true }),
  ).toBeVisible();
  await sheet.locator(".move-row select").selectOption("nf-alternate");
  await sheet.getByRole("button", { name: "Clear all" }).click();

  const guidance = sheet.getByRole("textbox", {
    name: "Filing guidance Optional",
  });
  await guidance.fill("Group weekly notes together");
  await sheet.getByRole("button", { name: "Replan" }).click();

  await expect(
    sheet.getByText("Fresh guided suggestion", { exact: true }),
  ).toBeVisible();
  await expect(sheet.getByText("Old suggestion", { exact: true })).toHaveCount(
    0,
  );
  await expect(sheet.locator(".move-row select")).toHaveValue("nf-weekly");
  await expect(sheet.getByRole("button", { name: "Apply (1)" })).toBeEnabled();
  await expect(guidance).toHaveValue("Group weekly notes together");

  const calls = await page.evaluate(
    () =>
      (globalThis as unknown as { __organizePlanCalls?: unknown[] })
        .__organizePlanCalls ?? [],
  );
  expect(calls).toEqual([
    { folderId: null, guidance: null },
    { folderId: null, guidance: "Group weekly notes together" },
  ]);
});

test("Notes Home keeps a pending replan open when Escape is pressed", async ({
  page,
}) => {
  await mockNotes(page, {
    plan_organize_notes: () => {
      const target = globalThis as unknown as {
        __notesPlanCalls?: number;
        __releaseNotesPlan?: () => void;
      };
      target.__notesPlanCalls = (target.__notesPlanCalls ?? 0) + 1;
      const plan = {
        scopeFolderId: null,
        moves: [
          {
            noteId: "n1",
            title: "Pending suggestion",
            fromFolderId: "nf1",
            fromFolder: "Notes",
            toFolder: "Ideas",
            toFolderId: null,
            reason: "An idea",
            confidence: "high",
          },
        ],
        totalScanned: 1,
        alreadyOrganized: 0,
        deferred: 0,
        targets: [],
      };
      if (target.__notesPlanCalls === 1) {
        return plan;
      }
      return new Promise((resolve) => {
        target.__releaseNotesPlan = () => resolve(plan);
      });
    },
  });
  await page.goto("/notes");
  await page.locator(".organize-btn").click();
  const sheet = page.getByRole("dialog", {
    name: "Review the auto-organize plan",
  });
  await sheet.getByRole("button", { name: "Replan" }).click();
  await expect(sheet.getByRole("button", { name: "Planning…" })).toBeDisabled();

  await page.keyboard.press("Escape");
  await expect(sheet).toBeVisible();

  await page.evaluate(() => {
    (
      globalThis as unknown as { __releaseNotesPlan?: () => void }
    ).__releaseNotesPlan?.();
  });
  await expect(sheet.getByRole("button", { name: "Replan" })).toBeEnabled();
});

test("privacy invalidation scrubs an open Notes organizer and drops its late replan", async ({
  page,
}) => {
  await mockNotes(page, {
    plan_organize_notes: (args: unknown) => {
      const target = globalThis as unknown as {
        __organizePlanCalls?: unknown[];
        __releaseNotesReplan?: () => void;
      };
      const calls = (target.__organizePlanCalls ??= []);
      calls.push(args);
      const plan = {
        scopeFolderId: null,
        moves: [
          {
            noteId: "n1",
            title: calls.length === 1 ? "Initial plan" : "Late plan",
            fromFolderId: "nf1",
            fromFolder: "Notes",
            toFolder: "Ideas",
            toFolderId: null,
            reason: "An idea",
            confidence: "high",
          },
        ],
        totalScanned: 1,
        alreadyOrganized: 0,
        deferred: 0,
        targets: [],
      };
      if (calls.length === 1) {
        return plan;
      }
      return new Promise((resolve) => {
        target.__releaseNotesReplan = () => resolve(plan);
      });
    },
  });
  await page.goto("/notes");
  await page.locator(".organize-btn").click();
  const sheet = page.locator("app-organize-sheet .sheet");
  await expect(sheet.getByText("Initial plan", { exact: true })).toBeVisible();

  await sheet
    .getByRole("textbox", { name: "Filing guidance Optional" })
    .fill("Sealed guidance phrase");
  await sheet.getByRole("button", { name: "Replan" }).click();
  await expect(sheet.getByRole("button", { name: "Planning…" })).toBeDisabled();
  // Planning disables the existing confirm label; it must not misreport that
  // an apply/move is in flight.
  await expect(sheet.getByRole("button", { name: "Apply (1)" })).toBeDisabled();
  await expect(sheet.getByRole("button", { name: "Applying…" })).toHaveCount(0);

  await page.evaluate(() => {
    (
      globalThis as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://ask-history-invalidated", null);
  });
  await expect(sheet).toBeHidden();
  await expect(page.getByText("Initial plan", { exact: true })).toHaveCount(0);
  await expect(
    page.getByText("Sealed guidance phrase", { exact: true }),
  ).toHaveCount(0);

  await page.evaluate(() => {
    (
      globalThis as unknown as { __releaseNotesReplan?: () => void }
    ).__releaseNotesReplan?.();
  });
  await expect(sheet).toBeHidden();
  await expect(page.getByText("Late plan", { exact: true })).toHaveCount(0);
});

test("privacy invalidation drops a late Notes organizer apply receipt", async ({
  page,
}) => {
  await mockNotes(page, {
    apply_organize_plan: () =>
      new Promise((resolve) => {
        (
          globalThis as unknown as {
            __releaseNotesApply?: () => void;
          }
        ).__releaseNotesApply = () =>
          resolve({
            appliedIds: [],
            failures: [
              {
                noteId: "n1",
                reason: "Late sealed failure detail",
                retryable: true,
              },
            ],
          });
      }),
  });
  await page.goto("/notes");
  await page.locator(".organize-btn").click();
  const sheet = page.locator("app-organize-sheet .sheet");
  await expect(sheet.getByText("My First Note", { exact: true })).toBeVisible();
  await sheet.getByRole("button", { name: "Apply (1)" }).click();
  await expect(sheet.getByRole("button", { name: "Applying…" })).toBeDisabled();

  await page.evaluate(() => {
    (
      globalThis as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://ask-history-invalidated", null);
  });
  await expect(sheet).toBeHidden();
  await expect(
    page
      .locator("app-organize-sheet")
      .getByText("My First Note", { exact: true }),
  ).toHaveCount(0);

  await page.evaluate(() => {
    (
      globalThis as unknown as { __releaseNotesApply?: () => void }
    ).__releaseNotesApply?.();
  });
  await expect(sheet).toBeHidden();
  await expect(
    page.getByText("Late sealed failure detail", { exact: true }),
  ).toHaveCount(0);
});

test("partial organize receipts stay cumulative while only retryable failures are retried", async ({
  page,
}) => {
  await mockNotes(page, {
    plan_organize_notes: () => ({
      scopeFolderId: null,
      moves: [
        {
          noteId: "n1",
          title: "Moved note",
          fromFolderId: "nf1",
          fromFolder: "Notes",
          toFolder: "Ideas",
          toFolderId: "nf-ideas",
          reason: "An idea",
          confidence: "high",
        },
        {
          noteId: "n2",
          title: "Retryable note",
          fromFolderId: "nf1",
          fromFolder: "Notes",
          toFolder: "Ideas",
          toFolderId: "nf-ideas",
          reason: "An idea",
          confidence: "medium",
        },
        {
          noteId: "n3",
          title: "Terminal note",
          fromFolderId: "nf1",
          fromFolder: "Notes",
          toFolder: "Archive",
          toFolderId: "nf-archive",
          reason: "Archive material",
          confidence: "low",
        },
        {
          noteId: "n4",
          title: "Unselected note",
          fromFolderId: "nf1",
          fromFolder: "Notes",
          toFolder: "Ideas",
          toFolderId: "nf-ideas",
          reason: "An idea",
          confidence: "low",
        },
      ],
      totalScanned: 6,
      alreadyOrganized: 1,
      deferred: 1,
      targets: [
        { id: "nf-ideas", label: "Notes / Ideas" },
        { id: "nf-archive", label: "Notes / Archive" },
      ],
    }),
    apply_organize_plan: (args: unknown) => {
      const target = globalThis as unknown as {
        __organizeApplyCalls?: unknown[];
      };
      const calls = (target.__organizeApplyCalls ??= []);
      calls.push(args);
      if (calls.length === 1) {
        return {
          appliedIds: ["n1"],
          failures: [
            {
              noteId: "n2",
              reason: "  invalid argument: destination is LOCKED \n ",
              retryable: true,
            },
            {
              noteId: "n3",
              reason: `Permanent policy refusal ${"detail ".repeat(80)}`,
              retryable: false,
            },
          ],
        };
      }
      return { appliedIds: ["n2"], failures: [] };
    },
  });
  await page.goto("/notes");
  await expect(page.locator(".notes-content")).toBeVisible();

  await page.locator(".organize-btn").click();
  const sheet = page.locator("app-organize-sheet .sheet");
  await expect(sheet).toBeVisible();
  await sheet
    .getByRole("checkbox", { name: "Include Unselected note" })
    .uncheck();
  await sheet.getByRole("button", { name: "Apply (3)" }).click();

  await expect(sheet).toBeVisible();
  await expect(sheet.getByText("4 proposed", { exact: true })).toBeVisible();
  await expect(sheet.getByText("1 moved.", { exact: true })).toBeVisible();
  await expect(
    sheet.getByText("2 still need attention.", { exact: true }),
  ).toBeVisible();
  await expect(sheet.getByText("Moved note", { exact: true })).toHaveCount(0);
  await expect(
    sheet.getByText("Retryable note", { exact: true }),
  ).toBeVisible();
  await expect(sheet.getByText("Terminal note", { exact: true })).toBeVisible();
  await expect(
    sheet.getByText("Unselected note", { exact: true }),
  ).toBeVisible();
  await expect(
    sheet.getByText("Unlock or choose an open destination, then retry.", {
      exact: true,
    }),
  ).toBeVisible();

  const terminalRow = sheet.locator(".move-row").filter({
    hasText: "Terminal note",
  });
  await expect(terminalRow.getByRole("checkbox")).toBeDisabled();
  await expect(terminalRow.locator("select")).toBeDisabled();
  const terminalFailure = terminalRow.locator(".row-status.is-failed");
  await expect(terminalFailure).toBeVisible();
  expect(
    (await terminalFailure.textContent())?.trim().length,
  ).toBeLessThanOrEqual(240);
  await expect(
    sheet.getByRole("checkbox", { name: "Include Unselected note" }),
  ).not.toBeChecked();
  await expect(page.locator(".toast.is-danger .toast-msg").last()).toHaveText(
    "1 moved; 2 still need attention.",
  );

  await sheet.getByRole("button", { name: "Apply (1)" }).click();

  // The retry succeeds, but the older terminal refusal remains visible and
  // keeps the review open. The deliberately-unselected row is not lost.
  await expect(sheet).toBeVisible();
  await expect(sheet.getByText("4 proposed", { exact: true })).toBeVisible();
  await expect(sheet.getByText("2 moved.", { exact: true })).toBeVisible();
  await expect(
    sheet.getByText("1 still need attention.", { exact: true }),
  ).toBeVisible();
  await expect(sheet.getByText("Retryable note", { exact: true })).toHaveCount(
    0,
  );
  await expect(sheet.getByText("Terminal note", { exact: true })).toBeVisible();
  await expect(
    sheet.getByText("Unselected note", { exact: true }),
  ).toBeVisible();
  await expect(sheet.getByRole("button", { name: /^Apply/ })).toHaveCount(0);
  await expect(page.locator(".toast.is-danger .toast-msg").last()).toHaveText(
    "1 moved; 1 still need attention.",
  );

  const calls = await page.evaluate(
    () =>
      (globalThis as unknown as { __organizeApplyCalls?: unknown[] })
        .__organizeApplyCalls ?? [],
  );
  expect(calls).toHaveLength(2);
  expect(
    (calls[0] as { plan: { moves: { noteId: string }[] } }).plan.moves.map(
      (move) => move.noteId,
    ),
  ).toEqual(["n1", "n2", "n3"]);
  expect(
    (calls[1] as { plan: { moves: { noteId: string }[] } }).plan.moves.map(
      (move) => move.noteId,
    ),
  ).toEqual(["n2"]);
});

test("a successful selected subset leaves unselected proposals in the review", async ({
  page,
}) => {
  await mockNotes(page, {
    plan_organize_notes: () => ({
      scopeFolderId: null,
      moves: [
        {
          noteId: "n1",
          title: "Selected note",
          fromFolderId: "nf1",
          fromFolder: "Notes",
          toFolder: "Ideas",
          toFolderId: "nf-ideas",
          reason: "An idea",
          confidence: "high",
        },
        {
          noteId: "n2",
          title: "Unselected note",
          fromFolderId: "nf1",
          fromFolder: "Notes",
          toFolder: "Ideas",
          toFolderId: "nf-ideas",
          reason: "An idea",
          confidence: "medium",
        },
      ],
      totalScanned: 2,
      alreadyOrganized: 0,
      deferred: 0,
      targets: [{ id: "nf-ideas", label: "Notes / Ideas" }],
    }),
    apply_organize_plan: () => ({ appliedIds: ["n1"], failures: [] }),
  });
  await page.goto("/notes");
  await page.locator(".organize-btn").click();
  const sheet = page.getByRole("dialog", {
    name: "Review the auto-organize plan",
  });
  await sheet
    .getByRole("checkbox", { name: "Include Unselected note" })
    .uncheck();
  await sheet.getByRole("button", { name: "Apply (1)" }).click();

  await expect(sheet).toBeVisible();
  await expect(sheet.getByText("Selected note", { exact: true })).toHaveCount(
    0,
  );
  const remaining = sheet.getByRole("checkbox", {
    name: "Include Unselected note",
  });
  await expect(remaining).toBeVisible();
  await expect(remaining).not.toBeChecked();
  await expect(sheet.getByText("2 proposed", { exact: true })).toBeVisible();
});
