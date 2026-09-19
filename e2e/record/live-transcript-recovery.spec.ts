import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

test("hidden live transcript offers gated unlock and preserves privacy after cancellation", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({ meetingId: "meeting-live" }),
    get_live_transcript_page: () => {
      if ((window as unknown as { liveLocked?: boolean }).liveLocked) throw new Error("locked: hidden");
      return { lines: [], truncated: false };
    },
    unlock_meeting: () => {
      const state = window as unknown as { unlockAttempts?: number; liveLocked: boolean };
      state.unlockAttempts = (state.unlockAttempts ?? 0) + 1;
      if (state.unlockAttempts === 1) throw new Error("auth: cancelled");
      state.liveLocked = false;
      return null;
    },
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  const panel = page.locator("app-live-transcript-panel");
  await page.evaluate(() => {
    const api = window as unknown as { liveLocked: boolean; __demoEmit: (event: string, payload: unknown) => void };
    api.__demoEmit("murmur://live-caption", { meetingId: "meeting-live", lineId: "private", speaker: "me", text: "Secret before lock", final: true, seq: 1 });
  });
  await expect(panel).toContainText("Secret before lock");
  await page.evaluate(() => {
    const api = window as unknown as { liveLocked: boolean; __demoEmit: (event: string, payload: unknown) => void };
    api.liveLocked = true;
    api.__demoEmit("murmur://ask-history-invalidated", null);
  });
  await expect(panel).not.toContainText("Secret before lock");
  const unlock = panel.getByRole("button", { name: "Unlock", exact: true });
  await unlock.click();
  await expect(panel.getByRole("alert")).toBeVisible();
  await expect(panel).toContainText("Live transcript is hidden");
  await unlock.click();
  await expect(panel).not.toContainText("Live transcript is hidden");
  await page.evaluate(() => {
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit("murmur://live-caption", { meetingId: "meeting-live", lineId: "fresh", speaker: "others", text: "New authorized speech", final: true, seq: 2 });
  });
  await expect(panel).toContainText("New authorized speech");
});

test("collapsed rail counts unread questions, not all lines, and record renders speech once", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({ meetingId: "meeting-live" }),
    get_live_transcript_page: () => ({ lines: [], truncated: false }),
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  const panel = page.locator("app-live-transcript-panel");
  await panel.getByRole("button", { name: "Collapse live transcript" }).click();
  await page.evaluate(() => {
    const emit = (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit;
    for (let seq = 1; seq <= 4; seq++) emit("murmur://live-caption", {
      meetingId: "meeting-live", lineId: `line-${seq}`, speaker: "others", text: `Speech ${seq}`, final: true, seq,
      possibleQuestion: seq === 3,
    });
  });
  await expect(panel.locator(".rail-count")).toHaveText("1");
  await panel.getByRole("button", { name: /Open live transcript/ }).click();
  await expect(panel.getByText("Speech 4", { exact: true })).toBeVisible();
  await expect(page.locator(".rec-foot")).not.toContainText("Speech 4");
  await expect(panel.locator(".panel-foot")).toContainText("4 lines");
  await panel.getByRole("button", { name: "Collapse live transcript" }).click();
  await expect(panel.locator(".rail-count")).toHaveCount(0);
});


test("runtime caption failure stays honest and keeps earlier history", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({ meetingId: "meeting-live" }),
    get_live_transcript_page: () => ({ lines: [], truncated: false }),
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  const panel = page.locator("app-live-transcript-panel");
  await page.evaluate(() => {
    const emit = (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit;
    emit("murmur://live-caption", { meetingId: "meeting-live", lineId: "before-error", speaker: "me", text: "Earlier speech remains", final: true, seq: 1 });
    emit("murmur://live-transcript-health", { meetingId: "meeting-live", others: "ready", captionsState: "retrying", modelLabel: "whisper:small", tickIntervalMs: 6000 });
  });
  await expect(panel).toContainText("Live captions are retrying. Recording continues.");
  await expect(panel.locator(".panel-foot")).toContainText("whisper:small · 6 s tick");
  await expect(panel).toContainText("Earlier speech remains");
  await page.evaluate(() => {
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit("murmur://live-transcript-health", { meetingId: "meeting-live", others: "unavailable", captionsState: "model-error" });
  });
  await expect(panel.getByRole("alert")).toContainText("Live captions stopped.");
  await expect(panel.getByRole("alert")).toContainText("Recording continues");
  await expect(panel.getByRole("link", { name: "Check caption settings" })).toBeVisible();
  await expect(panel).toContainText("Earlier speech remains");
  await expect(page.locator(".rec-foot")).toBeVisible();
  await expect(panel).not.toContainText("Me + Others");
});

test("a delayed replay cannot overwrite newer runtime caption failure", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    recording_status: () => ({ recording: true, meetingId: "meeting-live", startedAt: new Date().toISOString() }),
    get_live_transcript_page: () => new Promise(resolve => {
      (window as unknown as { resolveLivePage: (page: unknown) => void }).resolveLivePage = resolve;
    }),
  });
  await page.goto("/record");
  await expect(page.locator("app-live-transcript-panel")).toBeVisible();
  await page.evaluate(() => {
    const api = window as unknown as { __demoEmit: (event: string, payload: unknown) => void; resolveLivePage: (page: unknown) => void };
    api.__demoEmit("murmur://live-transcript-health", { meetingId: "meeting-live", others: "degraded", captionsState: "model-error" });
    api.resolveLivePage({ lines: [], truncated: false, othersState: "ready", captionsState: "ready" });
  });
  await expect(page.locator("app-live-transcript-panel").getByRole("alert")).toContainText("Live captions stopped.");
});

test("Start replays health emitted before recording status is visible", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({ meetingId: "meeting-live" }),
    get_live_transcript_page: () => ({ lines: [], truncated: false, othersState: "unavailable", captionsState: "model-error", modelLabel: "whisper:small", tickIntervalMs: 3000 }),
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  await expect(page.locator("app-live-transcript-panel").getByRole("alert")).toContainText("Live captions stopped.");
});

test("Restart captions retries the live worker without stopping or erasing earlier speech", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({ meetingId: "meeting-live" }),
    get_live_transcript_page: () => ({ lines: [], truncated: false, captionsState: (window as unknown as { restarted?: boolean }).restarted ? "ready" : "model-error", othersState: "ready" }),
    restart_live_captions: (args: Record<string, unknown>) => {
      const api = window as unknown as { restarted: boolean; __demoEmit: (event: string, payload: unknown) => void };
      if (args["meetingId"] !== "meeting-live") throw new Error("wrong recording");
      api.restarted = true;
      api.__demoEmit("murmur://live-transcript-health", { meetingId: "meeting-live", captionsState: "ready", others: "ready" });
    },
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  const panel = page.locator("app-live-transcript-panel");
  await expect(panel.getByRole("button", { name: "Restart captions" })).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit("murmur://live-caption", { meetingId: "meeting-live", lineId: "before-restart", speaker: "others", text: "Earlier committed speech", final: true, seq: 1 });
  });
  await panel.getByRole("button", { name: "Restart captions" }).click();
  await expect(panel).not.toContainText("Live captions stopped.");
  await expect(panel).toContainText("Earlier committed speech");
  await expect(page.locator(".stop-btn")).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit("murmur://live-caption", { meetingId: "meeting-live", lineId: "new-worker-line", speaker: "me", text: "New speech after restart", final: true, seq: 2 });
  });
  await expect(panel).toContainText("Earlier committed speech");
  await expect(panel).toContainText("New speech after restart");
});

test("Unlock cannot bypass a failed privacy listener barrier", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({ meetingId: "meeting-live" }),
    get_live_transcript_page: () => ({ lines: [{ meetingId: "meeting-live", lineId: "unsafe", speaker: "me", text: "Content without a privacy listener", final: true, seq: 1 }], truncated: false }),
    unlock_meeting: () => null,
  }, {}, ["murmur://ask-history-invalidated"]);
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  const panel = page.locator("app-live-transcript-panel");
  const unlock = panel.getByRole("button", { name: "Unlock", exact: true });
  // Reproduce the old reachable leak, while allowing the fixed distinct retry affordance.
  if (await unlock.count()) await unlock.click();
  else await panel.getByRole("button", { name: "Retry live transcript" }).click();
  await expect(panel).not.toContainText("Content without a privacy listener");
  await expect(panel.getByRole("button", { name: "Unlock", exact: true })).toHaveCount(0);
  await expect(panel).toContainText("Live transcript unavailable securely");
});
