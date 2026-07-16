import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * A note-save success must reflect the BACKEND's actual persist, not just local
 * RAM state.
 *
 * `MeetingConversationStore.acceptIntoNotes()` / `addNote()` both append the note
 * line + flip UI state synchronously, then call the private `persistNotes()`
 * helper, which fires `ipc.saveManualNotes(...)`. `save_manual_notes_inner`
 * (`commands.rs`) genuinely refuses with `AppError::Locked` when the folder's
 * session-unlock has lapsed between the click and the write (a concurrent
 * relock/seal, or the unlock window expiring). Before the fix, ANY rejection —
 * including `Locked` — was silently swallowed (`.catch(() => {})`), so the note
 * line stayed visible in the flow with NO error signal even though it was never
 * durably persisted (lost on next load/restart).
 *
 * RED contract: with `save_manual_notes` rejecting, the pre-fix code shows the
 * note line and nothing else — no toast — so `expect(toast).toBeVisible()` fails
 * against the unpatched store.
 */
test.describe("Record — a rejected note save surfaces an error, not a silent lie", () => {
  test("typing a note while save_manual_notes rejects (Locked) shows the note AND a danger toast", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({
        meetingId: "m-rec",
        startedAt: "2026-07-01T09:00:00Z",
      }),
      get_manual_notes: () => "",
      // Simulate the real backend gate refusing the write mid-session
      // (`AppError::Locked` from `save_manual_notes_inner`).
      save_manual_notes: () =>
        Promise.reject(
          "Locked: this meeting's folder is locked — unlock it to edit your notes",
        ),
      // The companion-note append is a SEPARATE, additive persist path; keep it
      // succeeding so this test isolates the `manual_notes` save-failure toast.
      append_to_companion_note: () => ({
        noteId: "n-companion",
        meetingWikilink: "[[Meeting]]",
      }),
    });

    await page.goto("/record");

    await page.locator("button.start-btn").click();
    await expect(page.locator("button.stop-btn")).toBeVisible({
      timeout: 10_000,
    });

    const composer = page.locator("mur-markdown-composer textarea.body-area");
    await expect(composer).toBeEnabled({ timeout: 10_000 });
    await composer.fill("ship the 3 flows people actually use");
    await composer.press("Enter");

    // The note line still renders locally (content is never destroyed)…
    await expect(
      page.getByText("ship the 3 flows people actually use"),
    ).toBeVisible();

    // …but the failed persist must be surfaced — never a silent swallow.
    await expect(page.locator(".toast.is-danger .toast-msg")).toBeVisible({
      timeout: 10_000,
    });
  });
});
