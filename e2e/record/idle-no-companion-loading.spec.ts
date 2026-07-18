import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * D2 (idle redesign, 2026-07-18). The base mock's config has `realtimeReactions:
 * true`, so the record screen at IDLE (nothing recording, no live meeting) is
 * exactly the bug path: `showAssistant()` used to be true on that setting ALONE,
 * mounting `app-meeting-conversation` → the embedded companion editor with a null
 * note id → a PERMANENT "Loading note…" spinner (the user's "I don't know why we
 * need this note" complaint).
 *
 * The fix gates the `realtimeReactions` term on an actual live meeting
 * (`store.meetingId()`), so idle shows the LAUNCH HERO (Start button) and NEVER
 * the companion surface / a loading note. RED before the guard (companion mounts +
 * "Loading note…" shows); GREEN after.
 */
test("idle record screen (realtimeReactions ON) shows the launch hero, not a Loading-note companion", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") consoleErrors.push(m.text());
  });
  page.on("pageerror", (e) => consoleErrors.push(String(e)));

  await mockTauri(page, { model_present: () => true });
  await page.goto("/record");

  // The launch hero (Start) is present — the idle surface is a launch surface.
  await expect(page.locator("button.start-btn")).toBeVisible();
  // With no live meeting, the companion surface is NOT mounted …
  await expect(page.locator("app-meeting-conversation")).toHaveCount(0);
  // … so a permanent "Loading note…" spinner can never appear at idle.
  await expect(page.getByText("Loading note…")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});
