import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Root-cause fix (2026-07-15): the note editor's autosave error handling used
 * to map EVERY non-`Locked` rejection to an opaque "Save failed · Retry" pill
 * with zero diagnostic — including a transient storage error a real backend
 * fix (a `busy_timeout` on the SQLCipher connection) now mitigates at the
 * source, but which the FE must still degrade gracefully for if it slips
 * through. This spec drives the FE-only defense-in-depth: one bounded
 * automatic retry for a transient failure, and a DISTINCT message for a
 * missing-note (`AppError::InvalidArg("no note {id}")`) rejection — which can
 * never succeed on retry, so it must not be treated like a generic error.
 */
test.describe("Note editor — save error handling surfaces the real diagnostic", () => {
  test("a transient save error is retried once and recovers silently (no red banner)", async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        consoleErrors.push(msg.text());
      }
    });
    page.on("pageerror", (err) => consoleErrors.push(String(err)));

    await mockNotes(page, {
      // First call rejects with a transient storage error (mirrors a busy-DB
      // collision); every call after that succeeds — the bounded ONE retry
      // must recover without ever showing the red "Save failed" pill.
      save_note_text: () => {
        const w = window as unknown as { __saveAttempts?: number };
        w.__saveAttempts = (w.__saveAttempts ?? 0) + 1;
        if (w.__saveAttempts === 1) {
          return Promise.reject("storage error: database is locked");
        }
        return Date.now();
      },
    });

    await page.goto("/notes");
    await page.locator(".title-btn", { hasText: "My First Note" }).click();

    const titleInput = page.locator(".note-title-input");
    await expect(titleInput).toHaveValue("My First Note");
    await titleInput.fill("");
    await titleInput.type("Recovers after retry");

    // The debounced autosave (600ms) fires, fails once, retries (800ms
    // backoff) and succeeds — never surfacing the red "Save failed" pill.
    const saveIndicator = page.locator(".save-state");
    await expect(saveIndicator).toHaveAttribute("data-state", "saved", {
      timeout: 5_000,
    });
    await expect(page.locator(".save-retry")).toHaveCount(0);

    expect(consoleErrors).toEqual([]);
  });

  test("a missing-note save error shows a distinct 'no longer exists' message, not the generic banner", async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        consoleErrors.push(msg.text());
      }
    });
    page.on("pageerror", (err) => consoleErrors.push(String(err)));

    await mockNotes(page, {
      // Mirrors `AppError::InvalidArg(format!("no note {id}"))` — the
      // stale-tab-after-delete case. Retrying this can never succeed.
      save_note_text: () => Promise.reject("invalid argument: no note n1"),
    });

    await page.goto("/notes");
    await page.locator(".title-btn", { hasText: "My First Note" }).click();

    const titleInput = page.locator(".note-title-input");
    await expect(titleInput).toHaveValue("My First Note");
    await titleInput.fill("");
    await titleInput.type("Edited a deleted note");

    // The distinct toast fires (not the generic "Save failed" banner alone).
    await expect(
      page.locator(".toast.is-danger .toast-msg", {
        hasText: /no longer exists/i,
      }),
    ).toBeVisible({ timeout: 5_000 });

    // The pill still shows "error" (so the user sees SOMETHING is wrong), but
    // its tooltip carries the distinct message rather than the raw backend
    // string, and retrying it is pointless (no auto-retry loop happened).
    const retryPill = page.locator(".save-retry");
    await expect(retryPill).toBeVisible();
    await expect(retryPill).toHaveAttribute("title", /no longer exists/i);

    expect(consoleErrors).toEqual([]);
  });
});
