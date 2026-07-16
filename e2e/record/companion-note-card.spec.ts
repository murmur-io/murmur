import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * A recording-time jot is now a REAL, LINKED note.
 *
 * `MeetingConversationStore.addNote()` optimistically appends the flow line, then
 * routes the markdown through `ipc.appendToCompanionNote(meetingId, markdown)` —
 * the backend lazily gets-or-creates the meeting's ONE living companion note and
 * returns `{ noteId, meetingWikilink }`. On resolve the store stamps that
 * reference onto the line (`saveState: "saved"`), and `NoteItemComponent` renders
 * a "✓ Saved to Notes" card footer: the primary affordance opens the companion
 * note by id, and a `🔗 [[Meeting]] →` chip navigates to the meeting.
 *
 * RED contract: before this change `addNote` only buffered into `manual_notes`
 * (no companion note, no card) — the "✓ Saved to Notes" affordance never existed,
 * so `getByRole("button", { name: /Saved to Notes/ })` finds nothing.
 */
test.describe("Record — a sent note becomes a linked companion note", () => {
  test("sending a jot renders the '✓ Saved to Notes' card with the meeting chip", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({
        meetingId: "m-rec",
        startedAt: "2026-07-01T09:00:00Z",
      }),
      get_manual_notes: () => "",
      save_manual_notes: () => null,
      // The append lazily creates + appends to the companion note and returns the
      // card reference (id to open by + the visible [[Meeting]] wikilink).
      append_to_companion_note: () => ({
        noteId: "n1",
        meetingWikilink: "[[Test Meeting]]",
      }),
    });

    await page.goto("/record");

    await page.locator("button.start-btn").click();
    await expect(page.locator("button.stop-btn")).toBeVisible({
      timeout: 10_000,
    });

    // The shared markdown composer replaced the plain textarea.
    const composer = page.locator("mur-markdown-composer textarea.body-area");
    await expect(composer).toBeEnabled({ timeout: 10_000 });
    await composer.fill("ship the three flows people actually use");
    await composer.press("Enter");

    // The note line renders…
    await expect(
      page.getByText("ship the three flows people actually use"),
    ).toBeVisible();

    // …and the companion-note card footer appears with the primary affordance
    // ("✓ Saved to Notes" visible text; aria-label "Open this note in Notes").
    await expect(
      page.getByRole("button", { name: /Open this note in Notes/ }),
    ).toBeVisible({ timeout: 10_000 });
    await expect(page.locator(".saved-open")).toContainText("Saved to Notes");

    // …plus the 🔗 [[Meeting]] chip labeled with the returned wikilink (brackets
    // stripped for display).
    await expect(page.locator(".meeting-chip .chip-name")).toHaveText(
      "Test Meeting",
    );
  });

  test("a failed append keeps the note line and shows a retry", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({
        meetingId: "m-rec",
        startedAt: "2026-07-01T09:00:00Z",
      }),
      get_manual_notes: () => "",
      save_manual_notes: () => null,
      // The append fails — the line must be KEPT (content never dropped) and the
      // card must surface a retry, not a silent loss.
      append_to_companion_note: () =>
        Promise.reject("Storage: could not write the companion note"),
    });

    await page.goto("/record");

    await page.locator("button.start-btn").click();
    await expect(page.locator("button.stop-btn")).toBeVisible({
      timeout: 10_000,
    });

    const composer = page.locator("mur-markdown-composer textarea.body-area");
    await expect(composer).toBeEnabled({ timeout: 10_000 });
    await composer.fill("a note whose append will fail");
    await composer.press("Enter");

    // The note line is never dropped even when the append fails.
    await expect(
      page.getByText("a note whose append will fail"),
    ).toBeVisible();

    // The failure is surfaced with a retry (never a silent swallow / lie).
    await expect(
      page.getByRole("button", { name: /Retry saving this note/ }),
    ).toBeVisible({ timeout: 10_000 });
  });
});
