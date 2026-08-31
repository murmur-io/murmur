import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const FOREST = [
  {
    id: "p-acme",
    name: "Acme",
    kind: "note",
    level: "project",
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
            id: "n-1",
            title: "Standup 14 Aug",
            durationS: null,
            sortAt: 1,
          },
        ],
      },
    ],
  },
  {
    id: "p-sealed",
    name: "Clients",
    kind: "note",
    level: "project",
    emoji: null,
    tint: null,
    locked: true,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
  {
    id: "p-session-unlocked",
    name: "Private notes",
    kind: "note",
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
            id: "n-private",
            title: "Private note",
            durationS: null,
            sortAt: 1,
          },
        ],
      },
    ],
  },
];

const PLAN = {
  scopeFolderId: "p-acme",
  moves: Array.from({ length: 30 }, (_, index) => ({
    noteId: `n-${index + 1}`,
    title: `Standup ${index + 1}`,
    fromFolderId: "p-acme",
    fromFolder: "Acme",
    toFolder: "Standups",
    toFolderId: "f-standups",
    reason: "A recurring team sync",
    confidence: index % 3 === 0 ? "low" : index % 2 === 0 ? "medium" : "high",
  })),
  totalScanned: 30,
  alreadyOrganized: 0,
  deferred: 0,
  targets: [
    { id: "f-standups", label: "Acme / Standups" },
    { id: "f-alternate", label: "Acme / Alternate" },
  ],
};

const REPLAN = {
  scopeFolderId: "p-acme",
  moves: [
    {
      noteId: "n-1",
      title: "Standup 1 — replanned",
      fromFolderId: "p-acme",
      fromFolder: "Acme",
      toFolder: "Client syncs",
      toFolderId: "f-client-syncs",
      reason: "The fresh guidance groups client syncs together",
      confidence: "high",
    },
    {
      noteId: "n-2",
      title: "Standup 2 — replanned",
      fromFolderId: "p-acme",
      fromFolder: "Acme",
      toFolder: "Client syncs",
      toFolderId: "f-client-syncs",
      reason: "The fresh guidance groups client syncs together",
      confidence: "medium",
    },
  ],
  totalScanned: 2,
  alreadyOrganized: 0,
  deferred: 0,
  targets: [{ id: "f-client-syncs", label: "Acme / Client syncs" }],
};

const PRIVATE_PLAN = {
  ...REPLAN,
  moves: [
    {
      ...REPLAN.moves[0],
      title: "SEALED CONTAINER ORGANIZER TITLE",
      reason: "SEALED CONTAINER ORGANIZER REASON",
    },
  ],
  totalScanned: 1,
};

const LATE_PRIVATE_PLAN = {
  ...PRIVATE_PLAN,
  moves: [
    {
      ...PRIVATE_PLAN.moves[0],
      title: "LATE SEALED CONTAINER TITLE",
      reason: "LATE SEALED CONTAINER REASON",
    },
  ],
};

async function open(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1280, height: 1000 });
  await mockTauri(
    page,
    {
      plan_organize_notes: (args: unknown) => {
        const w = globalThis as unknown as {
          __planned?: unknown[];
          __plan: unknown;
          __deferPlan?: boolean;
          __releasePlan?: () => void;
        };
        (w.__planned ??= []).push(args);
        if (w.__deferPlan) {
          return new Promise((resolve) => {
            w.__releasePlan = () => {
              w.__deferPlan = false;
              resolve(w.__plan);
            };
          });
        }
        return w.__plan;
      },
      apply_organize_plan: (args: unknown) => {
        const w = globalThis as unknown as {
          __applied?: unknown[];
          __applyResult?: unknown;
        };
        (w.__applied ??= []).push(args);
        return (
          w.__applyResult ?? {
            appliedIds: Array.from(
              { length: 29 },
              (_, index) => `n-${index + 1}`,
            ),
            failures: [
              {
                noteId: "n-30",
                reason: "Destination was locked",
                retryable: true,
              },
            ],
          }
        );
      },
    },
    { list_workspace_tree: FOREST },
  );
  await page.addInitScript((plan) => {
    (globalThis as unknown as { __plan: unknown }).__plan = plan;
  }, PLAN);
  await page.goto("/");
  await expect(page.getByRole("tree", { name: "Workspaces" })).toBeVisible();
}

/**
 * The AI organizer is scoped to the CONTAINER the user asked about.
 *
 * The planner and its review sheet already existed, reachable only from the Notes home header and
 * scoped to whichever note-folder happened to be active. The thing a user wants to tidy is a
 * project or a folder, and the hierarchy is where those are named — so the action moved to the
 * container's own actions menu, and the container id has to travel with it. A planner called with
 * the wrong scope reads the wrong notes and proposes moves for files the user was not looking at.
 */
test("organizing a container plans for THAT container", async ({ page }) => {
  await open(page);

  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.locator("[data-testid='organize-container']").click();

  const planned = await page.evaluate(
    () => (globalThis as unknown as { __planned?: unknown[] }).__planned ?? [],
  );
  expect(planned).toEqual([{ folderId: "p-acme", guidance: null }]);

  // Non-destructive: the review sheet is up and NOTHING has moved yet. An AI that silently
  // re-filed a vault would be a feature nobody could trust twice.
  await expect(
    page
      .locator(".move-row")
      .filter({ has: page.getByText("Standup 1", { exact: true }) })
      .locator("select"),
  ).toHaveValue("f-standups");
  await expect(page.getByText("30", { exact: true }).first()).toBeVisible();
  const appliedBeforeConfirm = await page.evaluate(
    () => (globalThis as unknown as { __applied?: unknown[] }).__applied ?? [],
  );
  expect(appliedBeforeConfirm).toEqual([]);
});

test("a fresh replan clears local edits and exclusions while preserving guidance", async ({
  page,
}) => {
  await open(page);
  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.locator("[data-testid='organize-container']").click();

  const firstMove = page
    .locator(".move-row")
    .filter({ has: page.getByText("Standup 1", { exact: true }) });
  await firstMove.locator("select").selectOption("f-alternate");
  await page.getByRole("button", { name: "Clear all" }).click();
  await page
    .getByRole("textbox", { name: "Filing guidance Optional" })
    .fill("Keep client syncs together");
  await page.evaluate((plan) => {
    (globalThis as unknown as { __plan: unknown }).__plan = plan;
  }, REPLAN);
  await page.getByRole("button", { name: "Replan" }).click();

  await expect(page.getByRole("button", { name: "Apply (2)" })).toBeVisible();
  await expect(
    page
      .locator(".move-row")
      .filter({
        has: page.getByText("Standup 1 — replanned", { exact: true }),
      })
      .locator("select"),
  ).toHaveValue("f-client-syncs");
  await expect(
    page.getByRole("textbox", { name: "Filing guidance Optional" }),
  ).toHaveValue("Keep client syncs together");
});

test("a pending replan disables apply and close until the current generation resolves", async ({
  page,
}) => {
  await open(page);
  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.locator("[data-testid='organize-container']").click();
  await page.evaluate((plan) => {
    const target = globalThis as unknown as {
      __plan: unknown;
      __deferPlan: boolean;
    };
    target.__plan = plan;
    target.__deferPlan = true;
  }, REPLAN);
  await page.getByRole("button", { name: "Replan" }).click();

  await expect(page.getByRole("button", { name: "Planning…" })).toBeDisabled();
  await expect(page.getByRole("button", { name: "Apply (30)" })).toBeDisabled();
  await expect(page.getByRole("button", { name: "Close" })).toBeDisabled();
  expect(
    await page.evaluate(
      () =>
        (globalThis as unknown as { __applied?: unknown[] }).__applied ?? [],
    ),
  ).toEqual([]);

  await page.evaluate(() =>
    (globalThis as unknown as { __releasePlan?: () => void }).__releasePlan?.(),
  );
  await expect(page.getByRole("button", { name: "Apply (2)" })).toBeEnabled();
});

test("privacy invalidation closes a container organizer and rejects its late replan", async ({
  page,
}) => {
  await open(page);
  await page.evaluate((plan) => {
    (globalThis as unknown as { __plan: unknown }).__plan = plan;
  }, PRIVATE_PLAN);
  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.locator("[data-testid='organize-container']").click();

  const sheet = page.getByRole("dialog", {
    name: "Review the auto-organize plan",
  });
  await expect(
    sheet.getByText("SEALED CONTAINER ORGANIZER TITLE", { exact: true }),
  ).toBeVisible();
  await expect(
    sheet.getByText("SEALED CONTAINER ORGANIZER REASON", { exact: true }),
  ).toBeVisible();

  await page.evaluate((plan) => {
    const target = globalThis as unknown as {
      __plan: unknown;
      __deferPlan?: boolean;
    };
    target.__plan = plan;
    target.__deferPlan = true;
  }, LATE_PRIVATE_PLAN);
  await sheet
    .getByRole("textbox", { name: "Filing guidance Optional" })
    .fill("Private filing guidance");
  await sheet.getByRole("button", { name: "Replan" }).click();
  await expect(sheet.getByRole("button", { name: "Planning…" })).toBeDisabled();

  await page.evaluate(() => {
    (
      globalThis as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://reminder-visibility-invalidated", null);
  });
  await expect(sheet).toHaveCount(0);
  await expect(
    page.getByText("SEALED CONTAINER ORGANIZER TITLE", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("SEALED CONTAINER ORGANIZER REASON", { exact: true }),
  ).toHaveCount(0);

  await page.evaluate(() => {
    (globalThis as unknown as { __releasePlan?: () => void }).__releasePlan?.();
  });
  await expect(
    page.getByText("LATE SEALED CONTAINER TITLE", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("LATE SEALED CONTAINER REASON", { exact: true }),
  ).toHaveCount(0);
  await expect(sheet).toHaveCount(0);
});

test("a successful container apply clears its busy review host", async ({
  page,
}) => {
  await open(page);
  await page.evaluate((plan) => {
    const target = globalThis as unknown as {
      __plan: unknown;
      __applyResult?: unknown;
    };
    target.__plan = plan;
    target.__applyResult = { appliedIds: ["n-1"], failures: [] };
  }, PRIVATE_PLAN);
  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.locator("[data-testid='organize-container']").click();

  const sheet = page.getByRole("dialog", {
    name: "Review the auto-organize plan",
  });
  await sheet.getByRole("button", { name: "Apply (1)" }).click();
  await expect(sheet).toHaveCount(0);
  await expect(page.locator(".toast.is-success .toast-msg")).toContainText(
    "1 note organized",
  );
});

test("guidance replans, 30/30 are accounted for, Clear all is absolute, and partial failures stay open", async ({
  page,
}) => {
  await open(page);
  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.locator("[data-testid='organize-container']").click();

  await expect(page.getByText("30 scanned")).toBeVisible();
  await expect(page.getByText("30 proposed")).toBeVisible();
  await page
    .getByRole("textbox", { name: "Filing guidance Optional" })
    .fill("Group recurring client syncs together");
  await page.getByRole("button", { name: "Replan" }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (globalThis as unknown as { __planned?: unknown[] }).__planned ?? [],
      ),
    )
    .toHaveLength(2);
  const planned = await page.evaluate(
    () => (globalThis as unknown as { __planned?: unknown[] }).__planned ?? [],
  );
  expect(planned[1]).toEqual({
    folderId: "p-acme",
    guidance: "Group recurring client syncs together",
  });

  await page.getByRole("button", { name: "Clear all" }).click();
  await expect(page.getByRole("button", { name: /Apply/ })).toHaveCount(0);
  await page.getByRole("button", { name: "Select all" }).click();
  await page.getByRole("button", { name: "Apply (30)" }).click();
  const applyCalls = await page.evaluate(
    () => (globalThis as unknown as { __applied?: unknown[] }).__applied ?? [],
  );
  expect(applyCalls).toHaveLength(1);
  expect(applyCalls[0]).toMatchObject({
    plan: {
      scopeFolderId: "p-acme",
      moves: Array.from({ length: 30 }, (_, index) => ({
        noteId: `n-${index + 1}`,
      })),
    },
  });
  expect(
    (
      applyCalls[0] as {
        plan: { moves: Array<Record<string, unknown>> };
      }
    ).plan.moves.every((move) => !("reviewScopeFolderId" in move)),
  ).toBe(true);
  await expect(page.getByText("Destination was locked")).toBeVisible();
  await expect(
    page.getByRole("dialog", { name: "Review the auto-organize plan" }),
  ).toBeVisible();
});

/**
 * A SEALED container does not offer the action.
 *
 * The planner reads titles and body excerpts to classify them, and those reads are gated — so for
 * a sealed container it can only ever return an empty plan. Offering an action that cannot do
 * anything is worse than not offering it: the user reads the empty result as the feature failing.
 */
test("a sealed container does not offer the organizer", async ({ page }) => {
  await open(page);

  await page.getByRole("button", { name: "Actions for Clients" }).click();
  await expect(page.locator("[data-testid='organize-container']")).toHaveCount(
    0,
  );
});

test("a session-unlocked but intrinsically locked container does not offer the organizer", async ({
  page,
}) => {
  await open(page);

  await page
    .getByRole("button", { name: "Actions for Private notes" })
    .click();
  await expect(page.locator("[data-testid='organize-container']")).toHaveCount(
    0,
  );
});

test("a retry toast reports cumulative unresolved failures from the reviewed plan", async ({
  page,
}) => {
  await open(page);
  await page.evaluate((plan) => {
    const target = globalThis as unknown as {
      __plan: unknown;
      __applyAttempt?: number;
      __applyResult?: unknown;
    };
    target.__plan = { ...plan, moves: plan.moves.slice(0, 2), totalScanned: 2 };
    target.__applyAttempt = 0;
    target.__applyResult = null;
  }, PLAN);
  await page.evaluate(() => {
    const target = globalThis as unknown as {
      __applyAttempt?: number;
      __applyResult?: unknown;
    };
    Object.defineProperty(target, "__applyResult", {
      configurable: true,
      get: () => {
        target.__applyAttempt = (target.__applyAttempt ?? 0) + 1;
        return target.__applyAttempt === 1
          ? {
              appliedIds: [],
              failures: [
                {
                  noteId: "n-1",
                  reason: "Source changed after planning",
                  retryable: false,
                },
                {
                  noteId: "n-2",
                  reason: "Destination was locked",
                  retryable: true,
                },
              ],
            }
          : { appliedIds: ["n-2"], failures: [] };
      },
    });
  });

  await page.getByRole("button", { name: "Actions for Acme" }).click();
  await page.locator("[data-testid='organize-container']").click();
  const sheet = page.getByRole("dialog", {
    name: "Review the auto-organize plan",
  });
  await sheet.getByRole("button", { name: "Apply (2)" }).click();
  await expect(sheet.getByText("2 still need attention.")).toBeVisible();

  await sheet.getByRole("button", { name: "Apply (1)" }).click();
  await expect(sheet.getByText("1 still need attention.")).toBeVisible();
  await expect(page.locator(".toast.is-danger .toast-msg").last()).toHaveText(
    "1 moved; 1 still need attention.",
  );
});
