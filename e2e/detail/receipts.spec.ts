import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Brain v3 PR-5 (Receipts) — END-TO-END FE wiring. The backend
 * (`get_note_receipts`, `meeting_is_unlocked`-gated + deterministic
 * `align_claims_to_segments`) is proven by `cargo test --lib`; this drives the
 * REAL Angular bundle (only the Tauri IPC boundary is mocked) to prove the
 * user-facing claim the adversarial verifier flagged as absent dead code:
 *
 *   - a receipt chip renders in the Note tab for each aligned BODY claim (a
 *     YAML front-matter line never chips, even if a stale backend aligns one),
 *   - the copy is honest: likely-source phrasing, ASR confidence as a TIER in
 *     the tooltip (never the raw float), h:mm:ss timestamps above 60 min,
 *   - clicking it switches to the Audio tab, seeks the player to the claim's
 *     second, and FLASHES the likely-source transcript segment
 *     (`.frag.is-flash`),
 *   - the seek is consumed ONE-SHOT (no replay on an Audio-tab revisit) while a
 *     repeat click on the SAME chip still re-fires (the `seq` re-arm).
 *
 * RED contract (fails on commit 43b7926, the unwired state): `getNoteReceipts`
 * had zero callers, `note-panel` had no `[receipts]` input and rendered no chip,
 * and `audio-panel` had no `[seekTarget]` input driving the flash — so the
 * `.receipt-chip` below never existed and the click had nothing to drive.
 * The PR-7 honesty assertions each failed RED on the pre-fix code too (bogus
 * front-matter chip, "Click to hear the moment this was said", raw-float-less
 * tier absent, "125:27", and the stale flash replay on revisit).
 */
test.describe("Detail — Receipts (claim → second of audio)", () => {
  test("a note claim's receipt chip seeks the Audio tab and flashes its segment", async ({
    page,
  }) => {
    await mockTauri(page, {
      // A meeting whose note has TWO claim lines the transcript proves — one
      // early (2:00) and one past the hour mark (2:05:27, the h:mm:ss form).
      // The mocked `get_note_receipts` mirrors the backend's shape: an alignment
      // per claim (claimIndex into `markdown.split('\n')`).
      get_meeting_detail: () => ({
        meeting: {
          id: "m-receipts",
          startedAt: "2026-07-01T09:00:00Z",
          endedAt: "2026-07-01T11:10:00Z",
          title: "Receipts meeting",
          durationS: 7800,
          audioPath: null,
          status: "EXPORTED",
          folderId: null,
        },
        note: {
          meetingId: "m-receipts",
          providerId: "claude_code",
          // Lines 0..=3 YAML front-matter, 4 heading, 5 blank, 6 + 7 the claims.
          markdown:
            "---\ntitle: Receipts meeting\nattendees: Anna, Bob\n---\n## Decisions\n\nWe will ship the redaction firewall next sprint.\nThe infra migration wrapped up late in the session.",
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
          {
            idx: 2,
            startS: 7527,
            endS: 7560,
            text: "The infra migration wrapped up late in the session.",
            speaker: "others",
          },
        ],
        assistantInteractions: [],
        locked: false,
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      // The Audio tab's side reads, with their REAL backend shapes (serde gives
      // `[]`/`false`, never undefined — the base mock's undefined fallback made
      // the timeline's `suggestionByLabel` computed throw "not iterable" during
      // CD on the revisit, killing rendering for the rest of the test).
      timeline_generation_on_device: () => false,
      suggest_speaker_labels: () => [],
      // The gated receipt read. A confident (0.92 → "audio: clear") receipt at
      // 2:00 and a shaky (0.4 → "audio: unclear") one past the hour mark — PLUS
      // a bogus alignment pointing at the `attendees:` FRONT-MATTER line (a
      // stale/older backend could emit one): the FE must never chip it.
      get_note_receipts: () => [
        {
          claimIndex: 2,
          segmentId: 1,
          startS: 120,
          endS: 150,
          speaker: "me",
          confidence: 0.9,
          overlap: 0.8,
        },
        {
          claimIndex: 6,
          segmentId: 1,
          startS: 120,
          endS: 150,
          speaker: "me",
          confidence: 0.92,
          overlap: 0.85,
        },
        {
          claimIndex: 7,
          segmentId: 2,
          startS: 7527,
          endS: 7560,
          speaker: "others",
          confidence: 0.4,
          overlap: 0.8,
        },
      ],
    });

    await page.goto("/meeting/m-receipts");

    // The Note tab is the default: ONLY the two body claims chip (the bogus
    // front-matter alignment is dropped), labelled with the claim snippet +
    // speaker + timestamp. The hint uses likely-source phrasing (never the
    // over-claiming "the moment this was said").
    const chip = page.locator("button.receipt-chip");
    await expect(chip).toHaveCount(2, { timeout: 10_000 });
    await expect(page.locator("button.receipt-chip", { hasText: "attendees" })).toHaveCount(0);
    await expect(page.locator(".receipts-hint")).toHaveText(
      "Jump to the likely source in the audio",
    );
    await expect(chip.first()).toContainText("redaction firewall");
    await expect(chip.first()).toContainText("Me");
    await expect(chip.first()).toContainText("2:00");

    // ASR confidence surfaces as a TIER in the tooltip — never the raw float.
    await expect(chip.first()).toHaveAttribute("title", /audio: clear/);
    await expect(chip.first()).not.toHaveAttribute("title", /0\.92/);
    await expect(chip.nth(1)).toHaveAttribute("title", /audio: unclear/);

    // Above 60 min the timestamp is h:mm:ss (7527s = 2:05:27), never "125:27".
    await expect(chip.nth(1)).toContainText("2:05:27");

    // Clicking a chip switches to the Audio tab (its panel is now mounted).
    await chip.first().click();
    await expect(page.getByRole("tab", { name: "Audio" })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    // The proving segment (idx 1) flashes — the wired-up `flashSegId` pulse.
    const flashed = page.locator(".frag.is-flash");
    await expect(flashed).toHaveCount(1, { timeout: 10_000 });
    await expect(flashed).toContainText("redaction firewall");

    // ONE-SHOT consumption: leaving the Audio tab and returning must NOT replay
    // the consumed seek — no stale flash on the recreated panel.
    await page.getByRole("tab", { name: "Note" }).click();
    await page.getByRole("tab", { name: "Audio" }).click();
    await expect(page.locator(".frag").first()).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator(".frag.is-flash")).toHaveCount(0);

    // …but a REPEAT CLICK on the SAME chip still re-fires (the seq re-arm).
    await page.getByRole("tab", { name: "Note" }).click();
    await chip.first().click();
    await expect(page.getByRole("tab", { name: "Audio" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await expect(page.locator(".frag.is-flash")).toHaveCount(1, {
      timeout: 10_000,
    });
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
