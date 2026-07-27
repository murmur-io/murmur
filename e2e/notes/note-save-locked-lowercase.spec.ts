import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * REGRESSION: the note editor's lock guard never fired, because it tested the WRONG CASE.
 *
 * `note-editor.component.ts::isUnretryableSaveError` tested `message.includes("Locked")` — capital
 * L. Every producer is lowercase: `AppError`'s `Display` is `#[error("locked: {0}")]`
 * (`src-tauri/src/error.rs`), and every note write-gate in `commands/notes.rs` goes through it. So
 * a save into a folder that sealed under the user (a screen-share auto-relock racing an autosave,
 * or "Lock all" while a note was open) was classified as RETRYABLE:
 *
 *   1. the bounded retry was scheduled and the user waited out an 800 ms backoff,
 *   2. the retry hit the identical lock refusal and failed identically,
 *   3. only then did the error settle — with the RAW backend string in the pill tooltip.
 *
 * The lock-specific toast ("This note is locked — unlock its folder to edit.") could never fire at
 * all, because its own `message.includes("Locked")` test had the same bug.
 *
 * The reason the existing suite did not catch it: the ONE spec that exercised this path
 * (`e2e/record/companion-note-flush-timeout.spec.ts`) rejected with a hand-written
 * `"Locked: companion note is sealed"` — capital L, a string the real backend never produces. The
 * fixture encoded the bug, so the test passed. It has been corrected to the real wire format in
 * the same change as this spec.
 *
 * RED CONTRACT: this rejects with the REAL lowercase, coded wire string. Against the pre-fix
 * component the lock arms never match, so `save_note_text` is called TWICE (the bounded retry) and
 * the locked toast never appears — both assertions below fail. Against the fixed component the
 * refusal is recognised on the first attempt, no retry is scheduled, and the toast fires.
 */

/*
 * THE WIRE STRING UNDER TEST — `AppError::Locked(errcode::tag(NOTE_LOCKED, …)).to_string()`:
 *
 *   "locked: [note-locked] unlock the folder to edit this note"
 *
 * It is INLINED in the override below rather than hoisted into a `const`, and must stay that way:
 * `mockTauri` serializes every override with `Function.prototype.toString()` and replays it
 * PAGE-SIDE, so an override that closes over a test-scope binding throws a `ReferenceError` in the
 * page instead of rejecting — which would make this spec fail for the wrong reason.
 */

test.describe("Note editor — a lowercase lock refusal is recognised, not retried", () => {
  test("a locked save fires the lock toast and is NOT retried", async ({
    page,
  }) => {
    await mockNotes(page, {
      save_note_text: () => {
        const w = window as unknown as { __saveAttempts?: number };
        w.__saveAttempts = (w.__saveAttempts ?? 0) + 1;
        // Inlined on purpose — see the note above the describe block.
        return Promise.reject(
          "locked: [note-locked] unlock the folder to edit this note",
        );
      },
    });

    await page.goto("/notes");
    await page.locator(".title-btn", { hasText: "My First Note" }).click();

    const titleInput = page.locator(".note-title-input");
    await expect(titleInput).toHaveValue("My First Note");
    await titleInput.fill("");
    await titleInput.type("Edited while the folder sealed");

    // The lock-specific toast — impossible to reach before the fix.
    await expect(
      page.locator(".toast.is-danger .toast-msg", {
        hasText: /locked — unlock its folder to edit/i,
      }),
    ).toBeVisible({ timeout: 5_000 });

    // NOT retried. The bounded retry uses an 800 ms backoff, so give it well past that and then
    // assert exactly one attempt — the whole point of classifying a lock refusal as unretryable.
    await page.waitForTimeout(2_000);
    const attempts = await page.evaluate(
      () => (window as unknown as { __saveAttempts?: number }).__saveAttempts ?? 0,
    );
    expect(attempts, "a lock refusal must not be retried").toBe(1);

    // The pill's tooltip carries the owned sentence, never the raw wire string.
    const retryPill = page.locator(".save-retry");
    await expect(retryPill).toBeVisible();
    const title = await retryPill.getAttribute("title");
    expect(title).not.toContain("[note-locked]");
    expect(title).not.toMatch(/^locked:/);
    expect(title).toMatch(/locked/i);
  });
});
