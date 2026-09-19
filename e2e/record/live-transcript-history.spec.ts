import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

test("reload snapshot final wins over an in-flight partial event", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    recording_status: () => ({ recording: true, meetingId: "meeting-live", startedAt: new Date().toISOString() }),
    get_live_transcript_page: () => new Promise((resolve) => {
      (window as unknown as { resolveLivePage: (value: unknown) => void }).resolveLivePage = resolve;
    }),
  });
  await page.goto("/record");
  const log = page.getByRole("log", { name: "Live transcript history" });
  await expect(log).toBeVisible();
  await page.evaluate(() => {
    const api = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
      resolveLivePage: (value: unknown) => void;
    };
    const line = { meetingId: "meeting-live", lineId: "others-1", speaker: "others", offsetMs: 1000 };
    api.__demoEmit("murmur://live-caption", { ...line, text: "Could you", final: false });
    api.resolveLivePage({ lines: [{ ...line, text: "Could you review the proposal?", final: true, seq: 1 }], truncated: false });
  });
  await expect(log).toContainText("Could you review the proposal?");
  await expect(log.locator(".is-partial")).toHaveCount(0);
});

test("unrelated invalidation resumes only after a fresh gated read; locked content stays hidden", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({ meetingId: "meeting-live" }),
    get_live_transcript_page: () => {
      if ((window as unknown as { liveLocked?: boolean }).liveLocked) throw new Error("locked: hidden");
      return { lines: [], truncated: false };
    },
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  const log = page.getByRole("log", { name: "Live transcript history" });
  await page.evaluate(() => {
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit("murmur://ask-history-invalidated", null);
  });
  await page.evaluate(() => {
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit("murmur://live-caption", {
      meetingId: "meeting-live", lineId: "fresh", speaker: "me", text: "Recording continues.", final: true, seq: 1,
    });
  });
  await expect(log).toContainText("Recording continues.");
  await page.evaluate(() => {
    const api = window as unknown as { liveLocked: boolean; __demoEmit: (event: string, payload: unknown) => void };
    api.liveLocked = true;
    api.__demoEmit("murmur://ask-history-invalidated", null);
  });
  await expect(log).not.toContainText("Recording continues.");
  await page.evaluate(() => {
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit("murmur://live-caption", {
      meetingId: "meeting-live", lineId: "late", speaker: "others", text: "Locked late content.", final: true, seq: 2,
    });
  });
  await expect(log).not.toContainText("Locked late content.");
});

test("history browsing always offers a return to live, including without new messages", async ({ page }) => {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({ meetingId: "meeting-live" }),
    get_live_transcript_page: () => ({ lines: [], truncated: false }),
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  await page.evaluate(() => {
    const emit = (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit;
    for (let seq = 1; seq <= 400; seq++) {
      emit("murmur://live-caption", {
        meetingId: "meeting-live", lineId: `line-${seq}`, speaker: "others",
        text: `Utterance ${seq}.`, offsetMs: seq * 1000, seq, final: true,
      });
    }
  });
  const log = page.getByRole("log", { name: "Live transcript history" });
  await expect(log).toContainText("Utterance 400.");
  await log.getByRole("button", { name: "Show earlier transcript" }).click();
  await expect(log).toContainText("Utterance 240.");
  await page.getByRole("button", { name: /Jump to latest/ }).click();
  await expect(log).toContainText("Utterance 400.");
  await page.evaluate(() => {
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit(
      "murmur://live-caption", {
        meetingId: "meeting-live", lineId: "line-401", speaker: "me",
        text: "The latest reply.", offsetMs: 401000, seq: 401, final: true,
      },
    );
  });
  await expect(log).toContainText("The latest reply.");
  await expect(log.locator("article")).toHaveCount(160);
});

test("live transcript retains earlier utterances while partials revise in place", async ({
  page,
}) => {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({ meetingId: "meeting-live" }),
    get_live_transcript_page: () => ({ lines: [], truncated: false }),
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  const log = page.getByRole("log", { name: "Live transcript history" });
  await expect(log).toBeVisible();

  await page.evaluate(() => {
    const emit = (window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
    }).__demoEmit;
    emit("murmur://live-caption", {
      meetingId: "meeting-live",
      lineId: "others-1",
      speaker: "others",
      text: "Could you send",
      offsetMs: 1_000,
      final: false,
    });
    emit("murmur://live-caption", {
      meetingId: "meeting-live",
      lineId: "others-1",
      speaker: "others",
      text: "Could you send the proposal?",
      offsetMs: 1_000,
      seq: 1,
      final: true,
      isQuestion: true,
      possibleQuestion: true,
    });
    emit("murmur://live-caption", {
      meetingId: "meeting-live",
      lineId: "me-1",
      speaker: "me",
      text: "Yes, this afternoon.",
      offsetMs: 2_000,
      seq: 2,
      final: true,
    });
    // A delayed partial must never undo a committed utterance.
    emit("murmur://live-caption", {
      meetingId: "meeting-live", lineId: "others-1", speaker: "others",
      text: "Could you send", offsetMs: 1_000, final: false,
    });
  });

  await expect(log).toContainText("Could you send the proposal?");
  await expect(log).toContainText("Yes, this afternoon.");
  await expect(log).not.toContainText("Could you sendCould you send");
  await expect(log.getByText("Possible question")).toBeVisible();

  await page.evaluate(() => {
    const emit = (window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
    }).__demoEmit;
    emit("murmur://ask-history-invalidated", null);
    emit("murmur://live-caption", {
      meetingId: "meeting-live",
      lineId: "late-after-lock",
      speaker: "others",
      text: "This must stay hidden after relock",
      seq: 3,
      final: true,
    });
  });

  await expect(log).not.toContainText("Could you send the proposal?");
  await expect(page.getByText("This must stay hidden after relock")).toHaveCount(0);
});
