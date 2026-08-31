import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const FOREST = Array.from({ length: 14 }, (_, index) => ({
  id: `space-${index + 1}`,
  name: index === 0 ? "Acme" : `Workspace ${index + 1}`,
  kind: "meeting",
  level: "project",
  emoji: null,
  tint: null,
  locked: false,
  unlocked: false,
  isRoot: false,
  folders: [],
  groups:
    index === 0
      ? [
          {
            kind: "note",
            total: 1,
            items: [
              {
                kind: "note",
                id: "note-1",
                title: "Launch brief",
                durationS: null,
                sortAt: 1,
              },
            ],
          },
        ]
      : [],
}));

const UNFILED = {
  kind: "meeting",
  total: 4,
  items: [
    {
      kind: "meeting",
      id: "meeting-1",
      title: "Platform standup",
      durationS: 1200,
      sortAt: 3,
    },
    {
      kind: "meeting",
      id: "meeting-2",
      title: "Hiring sync",
      durationS: 900,
      sortAt: 2,
    },
    {
      kind: "meeting",
      id: "meeting-3",
      title: "Audio only",
      durationS: 300,
      sortAt: 1,
    },
    {
      kind: "meeting",
      id: "meeting-4",
      title: "Processing audio",
      durationS: 120,
      sortAt: 0,
    },
  ],
  totalScanned: 4,
};

const PLAN = {
  moves: [
    {
      itemId: "meeting-1",
      title: "Platform standup",
      fromContainerId: null,
      fromContainer: "Unfiled",
      toContainerId: "space-1",
      toContainer: "Acme / Standups",
      reason: "Recurring engineering sync",
    },
    {
      itemId: "meeting-2",
      title: "Hiring sync",
      fromContainerId: null,
      fromContainer: "Unfiled",
      toContainerId: "space-2",
      toContainer: "Workspace 2 / Hiring",
      reason: "Hiring discussion",
    },
  ],
  skipped: [
    {
      itemId: "meeting-4",
      title: "Processing audio",
      code: "notReady",
      reason: "No generated note to classify",
    },
  ],
  review: [
    {
      itemId: "meeting-3",
      title: "Audio only",
      reason: "Two destinations look equally plausible",
      suggestedTargetId: "space-3",
      suggestedTarget: "Workspace 3 / Research",
    },
  ],
  targets: [
    { id: "space-1", label: "Acme / Standups" },
    { id: "space-2", label: "Workspace 2 / Hiring" },
    { id: "space-3", label: "Workspace 3 / Research" },
  ],
  totalScanned: 4,
};

async function openWorkspace(page: Page): Promise<string[]> {
  const runtimeErrors: string[] = [];
  page.on("pageerror", (error) => runtimeErrors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") {
      runtimeErrors.push(message.text());
    }
  });
  await page.setViewportSize({ width: 1280, height: 480 });
  await mockTauri(
    page,
    {
      create_folder: () => ({ id: "f-new" }),
      plan_workspace_organization: (args: unknown) => {
        const target = window as unknown as {
          __workspacePlanCalls?: number;
          __workspacePlanArgs?: unknown[];
          __plan: unknown;
        };
        target.__workspacePlanCalls = (target.__workspacePlanCalls ?? 0) + 1;
        (target.__workspacePlanArgs ??= []).push(args);
        return target.__plan;
      },
      apply_workspace_organization: (args: unknown) => {
        const target = window as unknown as {
          __workspaceApplyArgs?: unknown[];
          __workspaceApplyResult?: unknown;
          __deferWorkspaceApply?: boolean;
          __releaseWorkspaceApply?: () => void;
        };
        (target.__workspaceApplyArgs ??= []).push(args);
        const result =
          target.__workspaceApplyResult ??
          ({
            appliedIds: ["meeting-1"],
            failures: [
              {
                itemId: "meeting-3",
                reason: "Destination was locked",
                retryable: true,
              },
            ],
          } as const);
        if (target.__deferWorkspaceApply) {
          return new Promise((resolve) => {
            target.__releaseWorkspaceApply = () => {
              target.__deferWorkspaceApply = false;
              resolve(result);
            };
          });
        }
        return result;
      },
    },
    {
      list_workspace_tree: FOREST,
      list_container_items: UNFILED,
    },
  );
  await page.addInitScript((plan) => {
    (window as unknown as { __plan: unknown }).__plan = plan;
  }, PLAN);
  await page.goto("/");
  await expect(
    page.getByRole("navigation", { name: "Primary navigation" }),
  ).toBeVisible();
  return runtimeErrors;
}

test("workspace menus use the shared menu primitive and close after an action", async ({
  page,
}) => {
  const runtimeErrors = await openWorkspace(page);

  await page.getByRole("button", { name: "Actions for Acme" }).click();
  const item = page.getByRole("menuitem", { name: "Create folder here" });
  await expect(item).toBeVisible();
  expect(
    await item.evaluate((element) => {
      const style = getComputedStyle(element);
      return {
        display: style.display,
        background: style.backgroundColor,
        borderTopWidth: style.borderTopWidth,
        textAlign: style.textAlign,
      };
    }),
  ).toEqual({
    display: "flex",
    background: "rgba(0, 0, 0, 0)",
    borderTopWidth: "0px",
    textAlign: "left",
  });

  await item.click();
  await expect(
    page.getByRole("menuitem", { name: "Create folder here" }),
  ).toHaveCount(0);
  expect(runtimeErrors).toEqual([]);
});

test("the scroller snaps a partial first row below the fixed Workspaces header", async ({
  page,
}) => {
  const runtimeErrors = await openWorkspace(page);
  const body = page.locator(".primary-sidebar .sb-scroll");

  await body.evaluate((element) => {
    element.scrollTop = 56;
  });

  await expect
    .poll(() =>
      body.evaluate((element) => {
        const top = element.getBoundingClientRect().top;
        return Array.from(
          element.querySelectorAll<HTMLElement>(".tree > *"),
        ).filter((row) => {
          const rect = row.getBoundingClientRect();
          return rect.top < top && rect.bottom > top;
        }).length;
      }),
    )
    .toBe(0);
  expect(runtimeErrors).toEqual([]);
});

test("Brain reviews moves and skips, then applies only the selected recordings", async ({
  page,
}) => {
  const runtimeErrors = await openWorkspace(page);

  const organize = page.getByRole("button", {
    name: "Review filing moves with Brain",
  });
  await expect(organize).toBeEnabled();
  await organize.click();

  const sheet = page.getByRole("dialog", {
    name: "Review Brain filing plan",
  });
  await expect(sheet).toBeVisible();
  await expect(
    sheet.getByText("Platform standup", { exact: true }),
  ).toBeVisible();
  await expect(
    sheet
      .getByRole("checkbox", {
        name: "Move Platform standup to Acme / Standups",
      })
      .locator("xpath=ancestor::li"),
  ).toContainText(/Unfiled.*Acme \/ Standups/);
  await expect(sheet.getByText("Audio only", { exact: true })).toBeVisible();
  await expect(
    sheet.getByText("Brain's best match: Workspace 3 / Research"),
  ).toBeVisible();
  await expect(sheet.getByLabel("Destination for Audio only")).toHaveValue(
    "space-3",
  );
  await sheet.getByLabel("Destination for Audio only").selectOption("space-2");
  await sheet
    .getByRole("checkbox", {
      name: "Move Platform standup to Acme / Standups",
    })
    .uncheck();

  await sheet
    .getByRole("textbox", { name: "Filing guidance Optional" })
    .fill("Prefer client Workspaces over general folders");
  await sheet.getByRole("button", { name: "Replan" }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __workspacePlanArgs?: unknown[] })
            .__workspacePlanArgs ?? [],
      ),
    )
    .toEqual([
      { guidance: null },
      { guidance: "Prefer client Workspaces over general folders" },
    ]);
  await expect(sheet.getByLabel("Destination for Audio only")).toHaveValue(
    "space-3",
  );
  await expect(
    sheet.getByRole("checkbox", {
      name: "Move Platform standup to Acme / Standups",
    }),
  ).toBeChecked();
  await expect(
    sheet.getByRole("textbox", { name: "Filing guidance Optional" }),
  ).toHaveValue("Prefer client Workspaces over general folders");

  // Zero selection is an honest close-only state, never a dead "Apply (0)".
  await sheet.getByRole("button", { name: "Clear all" }).click();
  await expect(sheet.getByRole("button", { name: "Close" })).toBeVisible();
  await expect(
    sheet.getByRole("button", { name: /Move 0|Apply \(0\)/ }),
  ).toHaveCount(0);

  await sheet
    .getByRole("checkbox", {
      name: "Move Platform standup to Acme / Standups",
    })
    .check();
  await sheet.getByLabel("Destination for Audio only").selectOption("space-3");
  await sheet.getByRole("button", { name: "Move 2 recordings" }).click();

  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __workspaceApplyArgs?: unknown[] })
            .__workspaceApplyArgs ?? [],
      ),
    )
    .toEqual([
      {
        moves: [
          PLAN.moves[0],
          {
            itemId: "meeting-3",
            title: "Audio only",
            fromContainerId: null,
            fromContainer: "Unfiled",
            toContainerId: "space-3",
            toContainer: "Workspace 3 / Research",
            reason:
              "Destination chosen during review. Two destinations look equally plausible",
          },
        ],
      },
    ]);
  await expect(page.locator(".toast.is-danger .toast-msg")).toContainText(
    "1 recording organized; 1 still need attention",
  );
  await expect(sheet).toBeVisible();
  await expect(
    sheet
      .locator(".result-row.is-applied")
      .filter({ hasText: "Platform standup" }),
  ).toContainText("Filed in Acme / Standups");
  const failedResult = sheet
    .locator(".result-row.is-failed")
    .filter({ hasText: "Audio only" });
  await expect(failedResult).toContainText(
    "Couldn’t move to Workspace 3 / Research",
  );
  await expect(failedResult).toContainText("Destination was locked");
  await expect(
    sheet.getByRole("button", { name: "Retry 1 recording" }),
  ).toBeVisible();
  await expect(sheet.getByRole("button", { name: "Close" })).toBeVisible();
  expect(runtimeErrors).toEqual([]);
});

test("the visible Brain action exposes its planning state while the plan is pending", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      plan_workspace_organization: () =>
        new Promise((resolve) => {
          (
            window as unknown as { __releaseWorkspacePlan?: () => void }
          ).__releaseWorkspacePlan = () =>
            resolve({
              moves: [],
              review: [],
              skipped: [],
              targets: [],
              totalScanned: 0,
            });
        }),
    },
    { list_workspace_tree: FOREST, list_container_items: UNFILED },
  );
  await page.goto("/");

  const organize = page.getByRole("button", {
    name: "Review filing moves with Brain",
  });
  await expect(organize).toContainText("File recordings with Brain");
  await organize.click();
  await expect(organize).toContainText("Planning filing suggestions…");
  await expect(organize).toBeDisabled();

  await page.evaluate(() =>
    (
      window as unknown as { __releaseWorkspacePlan?: () => void }
    ).__releaseWorkspacePlan?.(),
  );
  await expect(organize).toContainText("File recordings with Brain");
});

test("privacy invalidation closes the global Brain organizer and rejects its late apply receipt", async ({
  page,
}) => {
  await openWorkspace(page);
  const privatePlan = {
    ...PLAN,
    moves: [
      {
        ...PLAN.moves[0],
        title: "SEALED GLOBAL ORGANIZER TITLE",
        reason: "SEALED GLOBAL ORGANIZER REASON",
      },
    ],
    review: [],
    skipped: [],
    totalScanned: 1,
  };
  await page.evaluate((plan) => {
    (window as unknown as { __plan: unknown }).__plan = plan;
  }, privatePlan);
  await page
    .getByRole("button", { name: "Review filing moves with Brain" })
    .click();

  const sheet = page.getByRole("dialog", { name: "Review Brain filing plan" });
  await expect(
    sheet.getByText("SEALED GLOBAL ORGANIZER TITLE", { exact: true }),
  ).toBeVisible();
  await expect(
    sheet.getByText("SEALED GLOBAL ORGANIZER REASON", { exact: true }),
  ).toBeVisible();
  await page.evaluate(() => {
    (
      window as unknown as { __deferWorkspaceApply?: boolean }
    ).__deferWorkspaceApply = true;
  });
  await sheet.getByRole("button", { name: "Move 1 recording" }).click();
  await expect(sheet.getByRole("button", { name: "Moving…" })).toBeDisabled();

  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://reminder-visibility-invalidated", null);
  });
  await expect(sheet).toHaveCount(0);
  await expect(
    page.getByText("SEALED GLOBAL ORGANIZER TITLE", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("SEALED GLOBAL ORGANIZER REASON", { exact: true }),
  ).toHaveCount(0);

  await page.evaluate(() => {
    (
      window as unknown as { __releaseWorkspaceApply?: () => void }
    ).__releaseWorkspaceApply?.();
  });
  await expect(sheet).toHaveCount(0);
  await expect(
    page.getByText("Destination was locked", { exact: true }),
  ).toHaveCount(0);
});

test("a completely successful filing closes the completed plan", async ({
  page,
}) => {
  const runtimeErrors = await openWorkspace(page);
  await page.evaluate(() => {
    (
      window as unknown as {
        __workspaceApplyResult?: unknown;
      }
    ).__workspaceApplyResult = {
      appliedIds: ["meeting-1", "meeting-2"],
      failures: [],
    };
  });

  await page
    .getByRole("button", { name: "Review filing moves with Brain" })
    .click();
  const sheet = page.getByRole("dialog", {
    name: "Review Brain filing plan",
  });
  await sheet.getByRole("button", { name: "Move 3 recordings" }).click();

  await expect(sheet).toHaveCount(0);
  await expect(page.locator(".toast.is-success .toast-msg")).toContainText(
    "2 recordings organized",
  );
  expect(runtimeErrors).toEqual([]);
});
