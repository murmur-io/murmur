import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

test("Reminders keep the picker default expand mode and persist flat dashboard sources", async ({
  page,
}) => {
  await mockTauri(page, {
    get_reminder_summary: () => ({ dueInboxCount: 0 }),
    list_reminders: () => ({
      inbox: [],
      upcoming: [],
      completed: [],
      dueInboxCount: 0,
    }),
    list_dashboards: () => [
      {
        id: "dashboard-reminder",
        title: "Reminder expansion board",
        emoji: "⏰",
        tileCount: 2,
        createdAt: "2026-08-06T01:00:00Z",
        updatedAt: "2026-08-06T01:00:00Z",
      },
    ],
    get_dashboard_sources: (args: { id: string }) => {
      const target = window as unknown as {
        __reminderDashboardReads?: unknown[];
      };
      target.__reminderDashboardReads = [
        ...(target.__reminderDashboardReads ?? []),
        args,
      ];
      return [
        { kind: "meeting", id: "m-dashboard-child", title: "Meeting child" },
        { kind: "note", id: "n-dashboard-child", title: "Note child" },
      ];
    },
    create_reminder: (args: unknown) => {
      (
        window as unknown as { __reminderCreateArgs?: unknown }
      ).__reminderCreateArgs = args;
      return {
        id: "reminder-created",
        title: "Expanded dashboard reminder",
        details: null,
        dueAt: Date.now() + 60_000,
        repeatEvery: null,
        repeatUnit: null,
        state: "active",
        origin: "manual",
        createdAt: Date.now(),
        updatedAt: Date.now(),
        completedAt: null,
        sources: [],
      };
    },
  });

  await page.goto("/reminders");
  await page.getByRole("button", { name: "New reminder" }).first().click();
  const composer = page.locator("app-reminder-composer");
  await composer.getByLabel("Title").fill("Expanded dashboard reminder");
  await composer.getByRole("button", { name: "+ Add source" }).click();
  await page
    .getByRole("option", { name: "Use dashboard Reminder expansion board" })
    .click();

  await expect(
    composer.locator('[data-testid="selected-dashboard-chip"]'),
  ).toHaveCount(0);
  await expect(composer.locator(".sp-chip-title")).toHaveText([
    "Meeting child",
    "Note child",
  ]);
  if (await page.locator(".sp-pop").isVisible()) {
    await page.locator(".sp-search").press("Escape");
  }
  await composer.getByRole("button", { name: "Create reminder" }).click();

  const state = await page.evaluate(() => ({
    reads:
      (window as unknown as { __reminderDashboardReads?: unknown[] })
        .__reminderDashboardReads ?? [],
    args: (
      window as unknown as { __reminderCreateArgs?: Record<string, unknown> }
    ).__reminderCreateArgs,
  }));
  expect(state.reads).toEqual([{ id: "dashboard-reminder" }]);
  expect(state.args).not.toHaveProperty("dashboardId");
  expect(
    (state.args?.["draft"] as Record<string, unknown>)?.["sources"],
  ).toEqual([
    { kind: "meeting", id: "m-dashboard-child" },
    { kind: "note", id: "n-dashboard-child" },
  ]);
});
