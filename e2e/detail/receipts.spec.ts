import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Brain v3 PR-5 (Receipts) — END-TO-END FE wiring. The backend
 * (`get_note_receipts`, `meeting_is_unlocked`-gated + deterministic
 * `align_claims_to_segments`) is proven by `cargo test --lib`; this drives the
 * REAL Angular bundle (only the Tauri IPC boundary is mocked) to prove the
 * user-facing claim the adversarial verifier flagged as absent dead code:
 *
 *   - a receipt chip renders in the Note tab for each aligned claim,
 *   - clicking it switches to the Audio tab, seeks the player to the claim's
 *     second, and FLASHES the proving transcript segment (`.frag.is-flash`).
 *
 * RED contract (fails on commit 43b7926, the unwired state): `getNoteReceipts`
 * had zero callers, `note-panel` had no `[receipts]` input and rendered no chip,
 * and `audio-panel` had no `[seekTarget]` input driving the flash — so the
 * `.receipt-chip` below never existed and the click had nothing to drive.
 */
test.describe("Detail — Receipts (claim → second of audio)", () => {
  test("a note claim's receipt chip seeks the Audio tab and flashes its segment", async ({
    page,
  }) => {
    await mockTauri(page, {
      // A meeting whose note has ONE claim line that the transcript proves.
      // The mocked `get_note_receipts` mirrors the backend's shape: an alignment
      // for that claim (claimIndex into `markdown.split('\n')`) → segment idx 1.
      get_meeting_detail: () => ({
        meeting: {
          id: "m-receipts",
          startedAt: "2026-07-01T09:00:00Z",
          endedAt: "2026-07-01T09:10:00Z",
          title: "Receipts meeting",
          durationS: 600,
          audioPath: null,
          status: "EXPORTED",
          folderId: null,
        },
        note: {
          meetingId: "m-receipts",
          providerId: "claude_code",
          // Line 0 heading, line 1 blank, line 2 the claim (matches segment 1).
          markdown:
            "## Decisions\n\nWe will ship the redaction firewall next sprint.",
          exportedPath: null,
        },
        segments: [
          {
            idx: 0,
            startS: 0,
            endS: 30,
            text: "Intro chatter before the decision.",
            speaker: "others",
          },
          {
            idx: 1,
            startS: 120,
            endS: 150,
            text: "We will ship the redaction firewall next sprint.",
            speaker: "me",
          },
        ],
        assistantInteractions: [],
        locked: false,
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      // The gated receipt read. Line index 2 = the claim; segment idx 1 at 120s.
      get_note_receipts: () => [
        {
          claimIndex: 2,
          segmentId: 1,
          startS: 120,
          endS: 150,
          speaker: "me",
          confidence: 0.92,
          overlap: 0.85,
        },
      ],
    });

    await page.goto("/meeting/m-receipts");

    // The Note tab is the default: the receipt chip renders, labelled with the
    // claim snippet + speaker + m:ss timestamp.
    const chip = page.locator("button.receipt-chip");
    await expect(chip).toHaveCount(1, { timeout: 10_000 });
    await expect(chip).toContainText("redaction firewall");
    await expect(chip).toContainText("Me");
    await expect(chip).toContainText("2:00");

    // Clicking it switches to the Audio tab (its panel is now mounted).
    await chip.click();
    await expect(page.getByRole("tab", { name: "Audio" })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    // The proving segment (idx 1) flashes — the wired-up `flashSegId` pulse.
    const flashed = page.locator(".frag.is-flash");
    await expect(flashed).toHaveCount(1, { timeout: 10_000 });
    await expect(flashed).toContainText("redaction firewall");
  });

  test("a locked meeting surfaces NO receipt chips (backend returns none)", async ({
    page,
  }) => {
    await mockTauri(page, {
      // A masked/locked meeting: the backend nulls the note + audio and the gated
      // `get_note_receipts` returns []. The Note tab must render no receipt chip.
      get_meeting_detail: () => ({
        meeting: {
          id: "m-locked",
          startedAt: "2026-07-01T09:00:00Z",
          endedAt: "2026-07-01T09:10:00Z",
          title: "🔒 Locked",
          durationS: 600,
          audioPath: null,
          status: "EXPORTED",
          folderId: "f-locked",
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        locked: true,
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      get_note_receipts: () => [],
    });

    await page.goto("/meeting/m-locked");
    // The lock gate renders; no receipt chip exists behind the lock.
    await expect(page.locator("button.receipt-chip")).toHaveCount(0, {
      timeout: 10_000,
    });
  });
});
