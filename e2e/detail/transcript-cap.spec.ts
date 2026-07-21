import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * P1 (render) — the transcript is windowed so a long (1h) meeting does not materialize thousands of
 * `<button>` fragments at once. `audio-panel.component` renders a hard-bounded window of at most
 * 80 turns / 160 fragments that always contains the karaoke-active turn, behind a "Show all"
 * expander.
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
      // The detail DTO now ships EMPTY segments (perf: transcript off the Note tab); the transcript
      // is fetched LAZILY via `get_meeting_segments` when the Audio tab opens. No audio (audioPath
      // null) keeps the test off the asset protocol; the transcript renders from the lazy segments.
      get_meeting_detail: () => ({
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
        segments: [],
        assistantInteractions: [],
        locked: false,
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      // A 100-turn meeting: alternating me/others so every segment is its own turn (turns fold
      // only CONSECUTIVE same-speaker segments). Built self-contained (overrides serialize page-
      // side and can't close over test-scope variables).
      get_meeting_segments: () => {
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
        return segments;
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

  test("keeps the cap bounded after a deep seek while rendering the active turn", async ({
    page,
  }) => {
    await mockTauri(page, {
      get_meeting_detail: () => ({
        meeting: {
          id: "m-atlas-roadmap",
          startedAt: "2026-07-01T09:00:00Z",
          endedAt: "2026-07-01T10:30:00Z",
          title: "Long deep-seek meeting",
          durationS: 5_000,
          audioPath: null,
          status: "EXPORTED",
          folderId: null,
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        locked: false,
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      // 1,000 alternating turns approximate a long dual-stream meeting. A receipt deep-link seeks
      // straight to the final turn; the transcript is fetched lazily on Audio-tab open (the seek is
      // deep-linked, which switches to the Audio tab). The old prefix window then did
      // `slice(0, activeIdx + 1)`, silently expanding the nominal cap to all 1,000 turns.
      get_meeting_segments: () => {
        const segments = [];
        for (let i = 0; i < 1_000; i++) {
          segments.push({
            idx: i,
            startS: i * 5,
            endS: i * 5 + 5,
            text: `Deep turn number ${i} content.`,
            speaker: i % 2 === 0 ? "me" : "others",
          });
        }
        return segments;
      },
    });

    await page.goto(
      "/meeting/m-atlas-roadmap?seekS=4995&seekSeg=999",
    );

    const turns = page.locator("li.turn");
    await expect(turns).toHaveCount(80, { timeout: 10_000 });
    await expect(page.locator('li.turn[data-turn="t999"]')).toHaveClass(
      /is-active/,
    );
    await expect(page.locator("button.frag.is-active")).toContainText(
      "Deep turn number 999",
    );
    await expect(
      page.getByRole("button", { name: /Show all 1000 turns/ }),
    ).toBeVisible();
  });

  test("prioritizes the exact receipt segment when many turns overlap its timestamp", async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.addInitScript(() => {
      Object.defineProperty(HTMLMediaElement.prototype, "currentTime", {
        configurable: true,
        get() {
          return (
            this as HTMLMediaElement & { __murmurTestTime?: number }
          ).__murmurTestTime ?? 0;
        },
        set(value: number) {
          (
            this as HTMLMediaElement & { __murmurTestTime?: number }
          ).__murmurTestTime = value;
        },
      });
      Object.defineProperty(HTMLMediaElement.prototype, "duration", {
        configurable: true,
        get: () => 6_000,
      });
      HTMLMediaElement.prototype.play = async () => undefined;
      HTMLMediaElement.prototype.pause = () => undefined;
    });
    await mockTauri(page, {
      get_meeting_detail: () => ({
        meeting: {
          id: "m-atlas-roadmap",
          startedAt: "2026-07-01T09:00:00Z",
          endedAt: "2026-07-01T10:30:00Z",
          title: "Long overlapping meeting",
          durationS: 6_000,
          audioPath: "/tmp/overlap.wav",
          status: "EXPORTED",
          folderId: null,
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        locked: false,
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      get_meeting_segments: () => {
        const segments = [];
        for (let i = 0; i < 1_200; i++) {
          segments.push({
            idx: i,
            startS: i * 5,
            endS: i === 0 ? 5_000 : i * 5 + 5,
            text: `Overlapping turn ${i}.`,
            speaker: i % 2 === 0 ? "me" : "others",
          });
        }
        return segments;
      },
    });

    await page.goto(
      "/meeting/m-atlas-roadmap?seekS=4995&seekSeg=999",
    );

    const turns = page.locator("li.turn");
    await expect(turns).toHaveCount(80, { timeout: 10_000 });
    await expect(page.locator('li.turn[data-turn="t999"]')).toBeVisible();
    await expect(page.locator('li.turn[data-turn="t0"]')).toHaveCount(0);
    const target = page.locator("button.frag.is-flash", {
      hasText: "Overlapping turn 999.",
    });
    await expect(target).toBeVisible();
    await expect(target).toHaveClass(/is-active/);

    // With reduced motion there is no animationend. Advancing beyond the exact
    // receipt segment must still release its static window anchor so karaoke
    // follows the live playhead instead of remaining pinned forever.
    await page.locator("audio").evaluate((audio) => {
      audio.currentTime = 5_200;
      audio.dispatchEvent(new Event("timeupdate"));
    });
    await expect(page.locator('li.turn[data-turn="t999"]')).toHaveCount(0);
    await expect(page.locator('li.turn[data-turn="t1040"]')).toHaveClass(
      /is-active/,
    );
    await expect(
      page.locator("button.frag.is-active", {
        hasText: "Overlapping turn 1040.",
      }),
    ).toBeVisible();
  });

  test("caps fragments for a long single-speaker run and still reveals the full transcript", async ({
    page,
  }) => {
    await mockTauri(page, {
      get_meeting_detail: () => ({
        meeting: {
          id: "m-atlas-roadmap",
          startedAt: "2026-07-01T09:00:00Z",
          endedAt: "2026-07-01T10:30:00Z",
          title: "Long monologue",
          durationS: 5_000,
          audioPath: null,
          status: "EXPORTED",
          folderId: null,
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        locked: false,
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      // A real single-speaker recording is the adversarial shape for turn-only windowing: all
      // consecutive segments fold into one turn, so a cap expressed only in turns still renders
      // every fragment and attaches every click handler.
      get_meeting_segments: () => {
        const segments = [];
        for (let i = 0; i < 1_000; i++) {
          segments.push({
            idx: i,
            startS: i * 5,
            endS: i * 5 + 5,
            text: `Monologue segment ${i}.`,
            speaker: "me",
          });
        }
        return segments;
      },
    });

    await page.goto("/meeting/m-atlas-roadmap");
    await page.getByRole("tab", { name: "Audio" }).click();

    const fragments = page.locator("button.frag");
    await expect(fragments).toHaveCount(160, { timeout: 10_000 });
    const cappedNodeCount = await page.locator(".audio-panel *").count();
    expect(cappedNodeCount).toBeLessThanOrEqual(500);

    // Search derives over every source segment/chunk, not just the current render window, so a
    // result at the very end remains discoverable without first expanding the transcript.
    await page.getByRole("textbox", { name: "Find in transcript" }).fill(
      "Monologue segment 999",
    );
    await expect(fragments).toHaveCount(8);
    await expect(fragments.last()).toContainText("Monologue segment 999");
    await page.getByRole("button", { name: "Clear search" }).click();
    await expect(fragments).toHaveCount(160);

    const showAll = page.getByRole("button", { name: /Show all/ });
    await expect(showAll).toBeVisible();
    await showAll.click();
    await expect(fragments).toHaveCount(1_000);
    const fullNodeCount = await page.locator(".audio-panel *").count();
    expect(fullNodeCount - cappedNodeCount).toBeGreaterThan(900);
  });
});
