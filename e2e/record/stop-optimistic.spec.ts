import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Long-meeting Stop UX — the Stop button must flip out of the recording state IMMEDIATELY on click.
 *
 * `stop_recording` runs the whole pipeline inline (transcribe the entire recording + generate the
 * note), which for a long meeting takes minutes. The store's `stop()` now sets `_stage` to
 * "transcribing" BEFORE awaiting that IPC (mirroring `resummarize`), so `isRecording()` goes false
 * at once: the recording strip (with the Stop button) is swapped for the processing view. Before the
 * fix, `_stage` stayed "recording" until the pipeline resolved — the Stop button kept rendering and
 * stayed clickable, so the UI looked frozen and a double-Stop was possible.
 *
 * RED contract: with `stop_recording` hanging (never resolving), the pre-fix code leaves the Stop
 * button visible → `toHaveCount(0)` fails.
 */
test.describe("Record — Stop flips to processing immediately", () => {
  test("clicking Stop swaps the recording strip for the processing view before the pipeline returns", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({
        meetingId: "m-rec",
        startedAt: "2026-07-01T09:00:00Z",
      }),
      // The whole transcribe+summarize pipeline runs inside this call — simulate a long-running
      // (never-resolving) invocation so the assertion lands DURING processing.
      stop_recording: () => new Promise(() => {}),
    });

    await page.goto("/record");

    // Start → the recording strip with the Stop button.
    await page.locator("button.start-btn").click();
    const stop = page.locator("button.stop-btn");
    await expect(stop).toBeVisible({ timeout: 10_000 });

    // Stop: the pipeline has NOT returned (mock hangs), yet the UI must leave the recording strip at
    // once — Stop button gone, processing view shown.
    await stop.click();
    await expect(stop).toHaveCount(0);
    await expect(
      page.getByText(/Transcribing|Summarizing|Exporting|Processing/i).first(),
    ).toBeVisible();
  });
});
