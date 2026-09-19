import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

test("Stop later waits for durable acknowledgement then offers the same-flow queue transition", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({ meetingId: "deferred-recording" }),
    stop_recording: (args) => {
      if (args.deferProcessing !== true) throw new Error("missing deferProcessing contract");
      return new Promise((resolve) => {
        (window as unknown as { resolveStopLater: () => void }).resolveStopLater = () => {
          (window as unknown as { __demoEmit: (name: string, value: unknown) => void }).__demoEmit("meetnotes://status", {
            stage: "idle", meetingId: "deferred-recording", message: "Recording saved to processing queue.",
          });
          resolve({ meetingId: "deferred-recording", processingDisposition: "queued" });
        };
      });
    },
    list_processing_queue: () => [],
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  await page.getByRole("button", { name: "Choose what happens after Stop" }).click();
  await page.getByRole("menuitemradio", { name: /Process later/ }).click();
  await page.getByRole("button", { name: "Stop recording and process later" }).click();
  await expect(page.getByText("Recording saved. It will wait on this Mac until you process it.")).toHaveCount(0);
  await page.waitForFunction(() => typeof (window as unknown as { resolveStopLater?: () => void }).resolveStopLater === "function");
  await page.evaluate(() => (window as unknown as { resolveStopLater: () => void }).resolveStopLater());
  await expect(page.getByText("Recording saved. It will wait on this Mac until you process it.")).toBeVisible();
  await expect(page.getByRole("button", { name: "Process now", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Open queue" }).click();
  await expect(page).toHaveURL(/\/queue$/);
  await expect(page.getByRole("heading", { name: "Processing queue" })).toBeVisible();
});
