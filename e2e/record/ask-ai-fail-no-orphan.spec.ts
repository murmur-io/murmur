import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * A failed voice-ask ("Ask AI") must NOT leave a permanent orphaned "🎙 …"
 * placeholder bubble in the notes/thread flow.
 *
 * `MeetingConversationStore.askNow()` optimistically pushes a fresh anchorless
 * thread — text "🎙 …", `persisted: false` — BEFORE awaiting
 * `ipc.beginVoiceCommand()`, so the listener orb can render immediately. When
 * that IPC call REJECTS (e.g. the brain backend is unavailable), the listener
 * never actually armed, so no voice-result event will ever land to backfill the
 * placeholder's text. Before the fix, the catch block only reset the
 * listening/in-flight signals and resolved the pending agent turn's error text
 * ("Couldn't start the listener.") — it never removed the placeholder note/
 * thread entry, so the raw "🎙 …" stayed rendered forever as the thread's first
 * user bubble (a note-item with `!persisted` renders `n.text` as the anchor
 * bubble) with no way to dismiss or retry it.
 *
 * RED contract: with `begin_voice_command` rejecting, the pre-fix code leaves
 * the "🎙 …" bubble (and the whole orphaned thread) permanently in the flow —
 * `toHaveCount(0)` on the mic-glyph bubble fails.
 */
test.describe("Record — a failed Ask AI leaves no orphaned mic bubble", () => {
  test("begin_voice_command rejecting removes the placeholder thread instead of stranding it", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({
        meetingId: "m-rec",
        startedAt: "2026-07-01T09:00:00Z",
      }),
      begin_voice_command: () => {
        throw new Error("brain backend unavailable");
      },
    });

    await page.goto("/record");

    await page.locator("button.start-btn").click();
    await expect(page.locator(".rec-topline")).toBeVisible({
      timeout: 10_000,
    });

    // Summon the Ask-Brain panel from the footer, then trigger a voice ask via its
    // mic — begin_voice_command rejects immediately (Calm-Notepad: voice now lives
    // inside the summoned panel, not a top-strip Ask button).
    await page.locator("app-record .ask-pill").click();
    const mic = page.locator("app-meeting-conversation .ask-panel .mic-btn");
    await expect(mic).toBeVisible();
    await mic.click();

    // The mic must NOT get stuck listening/processing.
    await expect(
      page.locator("app-meeting-conversation .mic-btn.is-listening"),
    ).toHaveCount(0);

    // No orphaned "🎙 …" bubble/thread should remain anywhere in the flow.
    await expect(page.getByText("🎙 …", { exact: true })).toHaveCount(0);

    // The flow stays in its pre-click state — no orphaned thread entry either.
    await expect(page.locator("app-note-item")).toHaveCount(0);
  });
});
