import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Webview-(re)load stage reconciliation — `RecorderStore.init()` now queries the
 * backend's `recording_status` once and reconciles the FE stage with the Rust
 * process's truth (the fix's FE half for the "pipeline future died / webview
 * swapped" wedge).
 *
 * Two shapes:
 *
 * 1. RED-able resync: the backend is GENUINELY recording (webview reloaded
 *    mid-recording — tauri-dev hot reload, Cmd-R, webview crash). Pre-fix the
 *    fresh store booted at "idle" (nothing ever called `recording_status`), so
 *    the surface showed the Start button while the backend recorder was live —
 *    and the next Start hit the "already recording" guard. Post-fix the surface
 *    boots straight into the recording strip.
 *
 * 2. The wedge's reload contract: the backend is NOT recording and the last
 *    meeting errored out (the pipeline task died before this webview session
 *    even loaded — its terminal events fired into the void). The (re)loaded
 *    record surface must NOT show any optimistic "Transcribing…" processing
 *    state — it settles on the idle surface, ready to record again.
 */
test.describe("Record — webview (re)load reconciles the stage with the backend", () => {
  test("a webview loaded while the backend is genuinely recording resyncs to the recording strip", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => true,
      recording_status: () => ({
        recording: true,
        meetingId: "m-live",
        startedAt: new Date(Date.now() - 90_000).toISOString(),
      }),
    });

    await page.goto("/record");

    // WITHOUT clicking Start: the store's init() resync must adopt the
    // backend's in-flight recording.
    await expect(page.locator(".rec-strip.is-recording")).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator("button.stop-btn")).toBeVisible();
    await expect(page.locator("button.start-btn")).toHaveCount(0);
  });

  test("a webview loaded after the pipeline died (backend idle, last meeting ERROR) never shows the optimistic transcribing state", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => true,
      // The backend's truth after the wedge: nothing is recording or
      // processing — the pipeline task is gone and the ghost meeting row was
      // marked ERROR by the terminal-status guard.
      recording_status: () => ({
        recording: false,
        meetingId: null,
        startedAt: null,
      }),
      list_meetings: () => [
        {
          id: "m-dead",
          startedAt: "2026-07-16T09:00:00Z",
          endedAt: null,
          title: "Wedged meeting",
          durationS: 1200,
          audioPath: null,
          status: "ERROR",
          folderId: null,
        },
      ],
    });

    await page.goto("/record");

    // The idle surface — never the processing view.
    await expect(page.locator("button.start-btn")).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByText(/Transcribing on-device/i)).toHaveCount(0);
    await expect(page.locator("button.stop-btn")).toHaveCount(0);
    await expect(page.locator(".rec-strip.is-recording")).toHaveCount(0);
  });
});
