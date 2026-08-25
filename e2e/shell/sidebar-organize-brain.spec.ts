import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const FOREST = Array.from({ length: 14 }, (_, index) => ({
  id: `space-${index + 1}`,
  name: index === 0 ? "Acme" : `Space ${index + 1}`,
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
      toContainer: "Space 2 / Hiring",
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
      suggestedTarget: "Space 3 / Research",
    },
  ],
  targets: [
    { id: "space-1", label: "Acme / Standups" },
    { id: "space-2", label: "Space 2 / Hiring" },
    { id: "space-3", label: "Space 3 / Research" },
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
      plan_workspace_organization: () => {
        const target = window as unknown as {
          __workspacePlanCalls?: number;
          __plan: unknown;
        };
        target.__workspacePlanCalls = (target.__workspacePlanCalls ?? 0) + 1;
        return target.__plan;
      },
      apply_workspace_organization: (args: unknown) => {
        const target = window as unknown as {
          __workspaceApplyArgs?: unknown[];
        };
        (target.__workspaceApplyArgs ??= []).push(args);
        return {
          appliedIds: ["meeting-1"],
          failures: [{ itemId: "meeting-9", reason: "Destination was locked" }],
        };
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
  await page.getByRole("button", { name: "Spaces" }).click();
  await expect(
    page.getByRole("complementary", { name: "Spaces sidebar" }),
  ).toBeVisible();
  return runtimeErrors;
}

test("workspace menus use the shared menu primitive and close after an action", async ({
  page,
}) => {
  const runtimeErrors = await openWorkspace(page);

  await page.getByRole("button", { name: "Add to Acme" }).click();
  const item = page.getByRole("menuitem", { name: "New folder" });
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
  await expect(page.getByRole("menuitem", { name: "New folder" })).toHaveCount(
    0,
  );
  expect(runtimeErrors).toEqual([]);
});

test("the scroller snaps a partial first row below the fixed Spaces header", async ({
  page,
}) => {
  const runtimeErrors = await openWorkspace(page);
  const body = page.locator(".spaces-sidebar .context-body");

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
  await expect(sheet.getByText("Platform standup", { exact: true })).toBeVisible();
  await expect(
    sheet
      .getByRole("checkbox", {
        name: "Move Platform standup to Acme / Standups",
      })
      .locator("xpath=ancestor::li"),
  ).toContainText(/Unfiled.*Acme \/ Standups/);
  await expect(sheet.getByText("Audio only", { exact: true })).toBeVisible();
  await expect(sheet.getByText("Brain's best match: Space 3 / Research")).toBeVisible();

  // Zero selection is an honest close-only state, never a dead "Apply (0)".
  await sheet.getByRole("button", { name: "Clear" }).click();
  await expect(sheet.getByRole("button", { name: "Close" })).toBeVisible();
  await expect(sheet.getByRole("button", { name: /Move 0|Apply \(0\)/ })).toHaveCount(0);

  await sheet.getByRole("checkbox", {
    name: "Move Platform standup to Acme / Standups",
  }).check();
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
            toContainer: "Space 3 / Research",
            reason:
              "Destination chosen during review. Two destinations look equally plausible",
          },
        ],
      },
    ]);
  await expect(page.locator(".toast.is-danger .toast-msg")).toContainText(
    "1 recording organized; 1 failed",
  );
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
          (window as unknown as { __releaseWorkspacePlan?: () => void }).__releaseWorkspacePlan =
            () => resolve({ moves: [], review: [], skipped: [], targets: [], totalScanned: 0 });
        }),
    },
    { list_workspace_tree: FOREST, list_container_items: UNFILED },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Spaces" }).click();

  const organize = page.getByRole("button", { name: "Review filing moves with Brain" });
  await expect(organize).toContainText("File recordings with Brain");
  await organize.click();
  await expect(organize).toContainText("Planning filing suggestions…");
  await expect(organize).toBeDisabled();

  await page.evaluate(() =>
    (window as unknown as { __releaseWorkspacePlan?: () => void }).__releaseWorkspacePlan?.(),
  );
  await expect(organize).toContainText("File recordings with Brain");
});
