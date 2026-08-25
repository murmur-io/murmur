import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const FOREST = Array.from({ length: 14 }, (_, index) => ({
  id: `space-${index + 1}`,
  name: index === 0 ? "Acme" : `Space ${index + 1}`,
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
  total: 3,
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
  ],
  totalScanned: 3,
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
      itemId: "meeting-3",
      title: "Audio only",
      reason: "No generated note to classify",
    },
  ],
  totalScanned: 3,
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
      create_folder: () => null,
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
    name: "Organize unfiled recordings with Brain",
  });
  await expect(organize).toBeEnabled();
  await organize.click();

  const sheet = page.getByRole("dialog", {
    name: "Review Brain organization plan",
  });
  await expect(sheet).toBeVisible();
  await expect(sheet.getByText("Platform standup", { exact: true })).toBeVisible();
  await expect(
    sheet.getByText("Unfiled → Acme / Standups", { exact: true }),
  ).toBeVisible();
  await expect(
    sheet.getByText("No generated note to classify", { exact: true }),
  ).toBeVisible();

  await page.getByRole("checkbox", { name: "Include Hiring sync" }).uncheck();
  await page.getByRole("button", { name: "Apply (1)" }).click();

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
        moves: [PLAN.moves[0]],
      },
    ]);
  await expect(page.locator(".toast.is-danger .toast-msg")).toContainText(
    "1 recording organized; 1 failed",
  );
  expect(runtimeErrors).toEqual([]);
});
