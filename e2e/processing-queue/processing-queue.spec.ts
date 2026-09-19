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
