import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * The final Saved card must reflect whether THIS note actually
 * exported, not merely whether a vault is configured globally.
 *
 * The backend legitimately returns `exported_path: null` for a specific note even
 * when a vault IS configured — e.g. the meeting's folder is locked, or a
 * resummarize lands on an already-sealed folder (`pipeline.rs` `run_after_stop`'s
 * `meeting_locked` branch). Before the fix, `record.component.ts`'s `hint()`
 * computed decided the "in the vault" vs "in Murmur" copy purely from
 * `vaultMissing()` (is a vault configured at all) and never consulted
 * `store.lastNote()?.exportedPath` (the real per-note result) — so a locked-folder
 * recording with a vault configured elsewhere cannot claim a vault export when no
 * `.md` file exists for it.
 *
 * RED contract: with a vault configured but `stop_recording`/`get_last_note`
 * returning `exportedPath: null`, the result must say it is saved in Murmur and
 * must not claim it was exported to the vault.
 */
test.describe("Record — Saved hint reflects the actual per-note export result", () => {
  test("a vault-configured but non-exported note (locked folder) shows the honest Murmur-only hint", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({ meetingId: "m-locked-folder" }),
      // Vault-configured, but THIS note's folder is locked so nothing exported —
      // mirrors pipeline.rs's `meeting_locked` branch (`exported_path: None`).
      stop_recording: () => ({
        meetingId: "m-locked-folder",
        markdown: "# Note\n\nBody.",
        exportedPath: null,
      }),
      get_last_note: () => ({
        meetingId: "m-locked-folder",
        providerId: "claude_code",
        markdown: "# Note\n\nBody.",
        exportedPath: null,
      }),
      get_meeting_detail: () => ({
        locked: true,
        meeting: {
          id: "m-locked-folder",
          startedAt: "2026-08-27T09:00:00Z",
          endedAt: "2026-08-27T09:01:00Z",
          title: "",
          durationS: 60,
          audioPath: null,
          status: "SUMMARIZED",
          folderId: "f-locked",
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
    });

    await page.goto("/record");

    await page.locator("button.start-btn").click();
    await expect(page.locator("button.stop-btn")).toBeVisible({
      timeout: 10_000,
    });
    await page.locator("button.stop-btn").click();

    const result = page.getByTestId("recording-result");
    await expect(result).toContainText(/Saved/, { timeout: 15_000 });

    // Honest copy: the note was NOT exported to the vault (locked folder), even
    // though a vault is configured — must say "in Murmur", never "in the vault".
    await expect(result).toContainText("Saved safely in Murmur");
    await expect(result).not.toContainText("exported to your vault");
  });
});
