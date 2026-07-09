import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * P1 (render) — the transcript is windowed so a long (1h) meeting does not materialize thousands of
 * `<button>` fragments at once. `audio-panel.component` renders the first `RENDER_CAP` (80) turns
 * (extended to include the karaoke-active turn) behind a "Show all N turns" expander.
 *
 * This drives the REAL FE bundle (only the Tauri IPC boundary is mocked) with a 100-turn meeting:
 * exactly 80 `li.turn` render, a "Show all" affordance offers the remaining 20, and clicking it
 * reveals all 100. RED contract: before the cap the `@for` iterated `visibleTurns()` → all 100
 * would render and there would be no "Show all" button.
 */
test.describe("Detail — transcript render cap (P1 virtualization)", () => {
  test("caps at 80 turns with a Show-all expander that reveals the rest", async ({
    page,
  }) => {
    await mockTauri(page, {
      // A 100-turn meeting: alternating me/others so every segment is its own turn (turns fold
      // only CONSECUTIVE same-speaker segments). No audio (audioPath null) keeps the test off the
      // asset protocol; the transcript renders from segments regardless.
      get_meeting_detail: () => {
        const segments = [];
        for (let i = 0; i < 100; i++) {
          segments.push({
            idx: i,
            startS: i * 5,
            endS: i * 5 + 5,
            text: `Turn number ${i} content.`,
            speaker: i % 2 === 0 ? "me" : "others",
          });
        }
        return {
          meeting: {
            id: "m-atlas-roadmap",
            startedAt: "2026-07-01T09:00:00Z",
            endedAt: "2026-07-01T09:50:00Z",
            title: "Long meeting",
            durationS: 3000,
            audioPath: null,
            status: "EXPORTED",
            folderId: null,
          },
          note: null,
          segments,
          assistantInteractions: [],
          locked: false,
          aiProvider: null,
          aiModel: null,
          modelServed: null,
        };
      },
    });

    await page.goto("/meeting/m-atlas-roadmap");
    await page.getByRole("tab", { name: "Audio" }).click();

    // Only the first RENDER_CAP (80) turns render.
    const turns = page.locator("li.turn");
    await expect(turns).toHaveCount(80, { timeout: 10_000 });

    // The expander offers the remaining 20.
    const showAll = page.getByRole("button", { name: /Show all 100 turns/ });
    await expect(showAll).toBeVisible();

    // Revealing renders all 100 and drops the expander.
    await showAll.click();
    await expect(turns).toHaveCount(100);
    await expect(showAll).toHaveCount(0);
  });
});
