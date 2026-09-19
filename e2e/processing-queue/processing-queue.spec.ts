import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

test("queue renders canonical states and masks held rows", async ({ page }) => {
  await mockTauri(
    page,
    {},
    {
      list_processing_queue: [
        {
          meetingId: "waiting-1",
          title: "Roadmap sync",
          state: "queued",
          position: 0,
          stage: null,
          attempts: 0,
          enqueuedAt: "2026-09-19T08:00:00Z",
          updatedAt: "2026-09-19T08:00:00Z",
          lastErrorCode: null,
          locked: false,
        },
        {
          meetingId: "locked-1",
          title: "this must never render",
          state: "queued",
          position: 1,
          stage: null,
          attempts: 0,
          enqueuedAt: "2026-09-19T09:00:00Z",
          updatedAt: "2026-09-19T09:00:00Z",
          lastErrorCode: null,
          locked: true,
        },
        {
          meetingId: "missing-lock-state",
          title: "missing lock state must fail closed",
          state: "queued",
          position: 2,
          stage: null,
          attempts: 0,
          enqueuedAt: "2026-09-19T10:00:00Z",
          updatedAt: "2026-09-19T10:00:00Z",
          lastErrorCode: null,
        },
      ],
    },
  );
  await page.goto("/queue");

  await expect(page.getByRole("heading", { name: "Processing queue" })).toBeVisible();
  await expect(page.getByText("Roadmap sync")).toBeVisible();
  await expect(page.getByText("🔒 Locked recording")).toHaveCount(2);
  await expect(page.getByText("this must never render")).toHaveCount(0);
  await expect(page.getByText("missing lock state must fail closed")).toHaveCount(0);
  const heldRows = page.locator(".queue-row.is-held");
  await expect(heldRows).toHaveCount(2);
  await expect(heldRows.locator('input[type="checkbox"]')).toHaveCount(2);
  await expect(heldRows.locator('input[type="checkbox"]').first()).toBeDisabled();
  await expect(heldRows.locator('input[type="checkbox"]').nth(1)).toBeDisabled();
  await expect(heldRows.getByRole("button", { name: /Process now|Retry|Remove/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Process now" }).first()).toBeEnabled();

  await page.getByRole("button", { name: /^Held/ }).click();
  await expect(page.getByText("🔒 Locked recording")).toHaveCount(2);
  await expect(page.getByText("Roadmap sync")).toHaveCount(0);
});

test("active queue run disables row and bulk mutations even outside its filter", async ({ page }) => {
  const rows = [
    { meetingId: "active", title: "Active", state: "processing" },
    { meetingId: "waiting-b", title: "Waiting B", state: "queued" },
    { meetingId: "waiting-c", title: "Waiting C", state: "queued" },
    { meetingId: "failed", title: "Failed recording", state: "failed" },
  ].map((row, position) => ({
    ...row, position, stage: null, attempts: 0, locked: false,
    enqueuedAt: "2026-09-19T08:00:00Z", updatedAt: "2026-09-19T08:00:00Z",
    lastErrorCode: null,
  }));
  await mockTauri(page, {}, { list_processing_queue: rows });
  await page.goto("/queue");
  await expect(page.getByText("Waiting B", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Move Waiting C up" })).toBeDisabled();
  for (const button of await page.getByRole("button", { name: /^(Process now|Retry|Remove)$/ }).all()) {
    await expect(button).toBeDisabled();
  }
  await page.getByRole("checkbox", { name: "Select Waiting B" }).check();
  const bulk = page.getByRole("toolbar", { name: "Selected queue items" });
  await expect(bulk.getByRole("button", { name: "Process now" })).toBeDisabled();
  await expect(bulk.getByRole("button", { name: "Remove" })).toBeDisabled();
  await page.getByRole("button", { name: /^Waiting \d/ }).click();
  await expect(page.getByText("Active", { exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Move Waiting C up" })).toBeDisabled();
});

test("worker latch keeps waiting-only queue mutations disabled between claims", async ({ page }) => {
  const rows = ["Waiting B", "Waiting C"].map((title, position) => ({
    meetingId: title, title, position, state: "queued", queueRunning: true,
    stage: null, attempts: 0, locked: false,
    enqueuedAt: "2026-09-19T08:00:00Z", updatedAt: "2026-09-19T08:00:00Z",
    lastErrorCode: null,
  }));
  await mockTauri(page, {}, { list_processing_queue: rows });
  await page.goto("/queue");
  await expect(page.getByText("Waiting C", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Move Waiting C up" })).toBeDisabled();
  for (const button of await page.getByRole("button", { name: /^(Process now|Remove)$/ }).all()) {
    await expect(button).toBeDisabled();
  }
  await page.getByRole("checkbox", { name: "Select Waiting B" }).check();
  const bulk = page.getByRole("toolbar", { name: "Selected queue items" });
  await expect(bulk.getByRole("button", { name: "Process now" })).toBeDisabled();
  await expect(bulk.getByRole("button", { name: "Remove" })).toBeDisabled();
});

test("worker release event clears busy even during a pending mutation refresh", async ({ page }) => {
  await mockTauri(page, {
    list_processing_queue: () => {
      const api = window as unknown as { queueStarted: boolean; queueReleased: boolean; resolveQueueRead: (rows: unknown[]) => void };
      const rows = [{
        meetingId: "waiting", title: "Waiting", position: 0, state: "queued",
        queueRunning: api.queueStarted && !api.queueReleased, locked: false,
        stage: null, attempts: 0, lastErrorCode: null, enqueuedAt: "now", updatedAt: "now",
      }];
      if (api.queueStarted && !api.queueReleased) {
        return new Promise((resolve) => { api.resolveQueueRead = resolve; });
      }
      return rows;
    },
    process_queue_now: () => {
      (window as unknown as { queueStarted: boolean }).queueStarted = true;
      return null;
    },
  });
  await page.goto("/queue");
  const process = page.getByRole("button", { name: "Process now", exact: true });
  await expect(process).toBeEnabled();
  await process.click();
  await page.waitForFunction(() => !!(window as unknown as { resolveQueueRead?: unknown }).resolveQueueRead);
  await page.evaluate(() => {
    const api = window as unknown as { queueReleased: boolean; __demoEmit: (event: string, payload: unknown) => void };
    api.queueReleased = true;
    api.__demoEmit("murmur://processing-queue-changed", null);
  });
  await expect(page.locator(".refreshing")).toHaveCount(0);
  await page.evaluate(() => {
    (window as unknown as { resolveQueueRead: (rows: unknown[]) => void }).resolveQueueRead([{
      meetingId: "waiting", title: "Waiting", position: 0, state: "queued", queueRunning: true,
      locked: false, stage: null, attempts: 0, lastErrorCode: null, enqueuedAt: "now", updatedAt: "now",
    }]);
  });
  await expect(process).toBeEnabled();
});
