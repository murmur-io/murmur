import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const FOREST = [
  {
    id: "space-1",
    name: "Acme",
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
];

const PLAN = {
  moves: [
    {
      itemId: "meeting-alpha",
      title: "Alpha sync",
      fromContainerId: null,
      fromContainer: "Unfiled",
      toContainerId: "space-1",
      toContainer: "Acme / Standups",
      reason: "Recurring engineering sync",
    },
    {
      itemId: "meeting-beta",
      title: "Beta review",
      fromContainerId: null,
      fromContainer: "Unfiled",
      toContainerId: "space-1",
      toContainer: "Acme / Reviews",
      reason: "Product review",
    },
  ],
  review: [
    {
      itemId: "meeting-gamma",
      title: "Gamma planning",
      reason: "Two destinations look plausible",
      suggestedTargetId: "space-1",
      suggestedTarget: "Acme / Planning",
    },
  ],
  skipped: [
    {
      itemId: "meeting-processing",
      title: "Processing audio",
      code: "notReady",
      reason: "The generated note is not ready yet",
    },
  ],
  targets: [{ id: "space-1", label: "Acme / Planning" }],
  totalScanned: 4,
};

async function openOrganizer(
  page: Page,
  scenario: "partial-then-success" | "all-fail",
): Promise<string[]> {
  const runtimeErrors: string[] = [];
  page.on("pageerror", (error) => runtimeErrors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") {
      runtimeErrors.push(message.text());
    }
  });
  await mockTauri(
    page,
    {
      plan_workspace_organization: () =>
        (window as unknown as { __organizePlan: unknown }).__organizePlan,
      apply_workspace_organization: (args: {
        moves: { itemId: string }[];
      }) => {
        const target = window as unknown as {
          __organizeApplyCalls?: unknown[];
          __organizeScenario: "partial-then-success" | "all-fail";
        };
        (target.__organizeApplyCalls ??= []).push(args);
        if (target.__organizeScenario === "all-fail") {
          return {
            appliedIds: [],
            failures: args.moves.map((move) => ({
              itemId: move.itemId,
              reason: "invalid argument: destination folder is locked",
            })),
          };
        }
        if (target.__organizeApplyCalls.length === 1) {
          return {
            appliedIds: ["meeting-alpha"],
            failures: [
              {
                itemId: "meeting-gamma",
                reason: "invalid argument: destination folder is locked",
              },
            ],
          };
        }
        return { appliedIds: ["meeting-gamma"], failures: [] };
      },
    },
    {
      list_workspace_tree: FOREST,
      list_container_items: {
        kind: "meeting",
        total: 4,
        items: [],
      },
    },
  );
  await page.addInitScript(
    ({ plan, applyScenario }) => {
      const target = window as unknown as {
        __organizePlan: unknown;
        __organizeScenario: "partial-then-success" | "all-fail";
      };
      target.__organizePlan = plan;
      target.__organizeScenario = applyScenario;
    },
    { plan: PLAN, applyScenario: scenario },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Spaces" }).click();
  await page.getByRole("button", { name: "Review filing moves with Brain" }).click();
  await expect(
    page.getByRole("dialog", { name: "Review Brain filing plan" }),
  ).toBeVisible();
  return runtimeErrors;
}

test("a partial failure has one state per item and retries only the selected failure", async ({
  page,
}) => {
  const runtimeErrors = await openOrganizer(page, "partial-then-success");
  const sheet = page.getByRole("dialog", { name: "Review Brain filing plan" });

  await sheet
    .getByRole("checkbox", { name: "Move Beta review to Acme / Reviews" })
    .uncheck();
  await sheet
    .getByLabel("Destination for Gamma planning")
    .selectOption("space-1");
  await sheet.getByRole("button", { name: "Move 2 recordings" }).click();

  await expect(sheet.getByText("Alpha sync", { exact: true })).toHaveCount(1);
  await expect(sheet.getByText("Gamma planning", { exact: true })).toHaveCount(1);
  await expect(
    sheet.getByRole("checkbox", {
      name: "Move Gamma planning to Acme / Planning",
    }),
  ).toHaveCount(0);
  const failed = sheet.locator(".result-row.is-failed");
  await expect(failed).toContainText(
    "Unlock or choose an open destination, then retry",
  );
  await expect(failed).not.toContainText("invalid argument:");
  await expect(
    sheet.getByRole("heading", { name: "Left unchanged" }),
  ).toBeVisible();
  await expect(sheet).toContainText(
    "1 recording was not included in this attempt. It remains unfiled",
  );
  await expect(
    sheet.locator(
      'input[type="checkbox"]:enabled:not([aria-label^="Retry "])',
    ),
  ).toHaveCount(0);
  await expect(sheet.getByText("Processing audio", { exact: true })).toHaveCount(0);
  await expect(sheet.getByRole("button", { name: /Still processing.*1/ })).toBeVisible();

  const retrySelection = sheet.getByRole("checkbox", {
    name: "Retry Gamma planning",
  });
  await expect(retrySelection).toBeChecked();
  await retrySelection.uncheck();
  await expect(sheet.getByRole("button", { name: /^Retry/ })).toHaveCount(0);
  await retrySelection.check();
  await sheet.getByRole("button", { name: "Retry 1 recording" }).click();

  await expect(sheet).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __organizeApplyCalls?: unknown[] })
            .__organizeApplyCalls ?? [],
      ),
    )
    .toEqual([
      { moves: [PLAN.moves[0], {
        itemId: "meeting-gamma",
        title: "Gamma planning",
        fromContainerId: null,
        fromContainer: "Unfiled",
        toContainerId: "space-1",
        toContainer: "Acme / Planning",
        reason:
          "Destination chosen during review. Two destinations look plausible",
      }] },
      {
        moves: [
          {
            itemId: "meeting-gamma",
            title: "Gamma planning",
            fromContainerId: null,
            fromContainer: "Unfiled",
            toContainerId: "space-1",
            toContainer: "Acme / Planning",
            reason:
              "Destination chosen during review. Two destinations look plausible",
          },
        ],
      },
    ]);
  expect(runtimeErrors).toEqual([]);
});

test("all failures stay in one compact attention group while waiting items remain collapsed", async ({
  page,
}) => {
  const runtimeErrors = await openOrganizer(page, "all-fail");
  const sheet = page.getByRole("dialog", { name: "Review Brain filing plan" });

  await sheet.getByRole("button", { name: "Move 2 recordings" }).click();

  await expect(sheet.locator(".result-row.is-failed")).toHaveCount(2);
  await expect(sheet.getByText("Alpha sync", { exact: true })).toHaveCount(1);
  await expect(sheet.getByText("Beta review", { exact: true })).toHaveCount(1);
  await expect(
    sheet.getByRole("heading", { name: "Ready to file", exact: true }),
  ).toHaveCount(0);
  await expect(sheet.getByText("Processing audio", { exact: true })).toHaveCount(0);
  await expect(sheet.getByRole("button", { name: /Still processing.*1/ })).toBeVisible();
  await expect(sheet).not.toContainText("invalid argument:");
  await expect(
    sheet.getByRole("button", { name: "Retry 2 recordings" }),
  ).toBeVisible();
  expect(runtimeErrors).toEqual([]);
});
