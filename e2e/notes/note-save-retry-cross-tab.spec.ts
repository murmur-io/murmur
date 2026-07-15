import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Regression for a CRITICAL finding from adversarial review of PR #332: the
 * bounded save-retry (`NoteEditorComponent.retryOnce`) is scheduled through
 * `DebounceService`, a `providedIn: 'root'` SINGLETON shared by every open
 * `NoteEditorComponent` instance. `notes/:id` tabs stay ALIVE-BUT-DETACHED
 * when backgrounded (`TabRouteReuseStrategy.shouldDetach`/`shouldAttach`), so
 * with two note tabs open, a bare literal debounce key
 * (`"note-editor-save-retry"`) let a SECOND note's failed-save retry
 * `clearTimeout` a FIRST note's still-pending retry (`DebounceService`
 * coalesces same-key calls) — the first note's `retryOnce` promise then
 * never settled, leaving its `saveState` stuck on `"saving"` forever with no
 * path to `"saved"` or `"error"`.
 *
 * The fix scopes the debounce key per note id
 * (`` `note-editor-save-retry:${noteId}` ``) so two open tabs get independent
 * retry slots in the shared singleton.
 *
 * RED contract: reverting the per-id scoping back to the bare literal key
 * reproduces Note A's save-state wedged on "saving" forever once Note B's
 * retry is scheduled inside Note A's still-pending backoff window (confirmed
 * locally against the unscoped-key code before landing this test).
 */
test.describe("Note editor — concurrent open tabs never cancel each other's save retry", () => {
  test("two notes with a transient save failure each independently reach 'saved', not stuck on 'saving'", async ({
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
      // The shared mock's `get_note` ignores `args.id` and always returns
      // "My First Note" — fine for single-note specs, but this test needs
      // n1 and n2 to be genuinely distinct notes, so override per-id here.
      get_note: (args: { id: string }) =>
        args.id === "n2"
          ? {
              id: "n2",
              title: "Weekly plan",
              folderId: "nf1",
              markdown: "# Weekly plan\n\nShip the notes feature.",
              tags: [],
              properties: {},
              updatedAt: 1_720_100_000_000,
              createdAt: 1_719_100_000_000,
              exportedPath: null,
              locked: false,
              shared: true,
            }
          : {
              id: "n1",
              title: "My First Note",
              folderId: "nf1",
              markdown: "# Heading\n\nSome body text to select.",
              tags: ["idea"],
              properties: {},
              updatedAt: 1_720_000_000_000,
              createdAt: 1_719_000_000_000,
              exportedPath: null,
              locked: false,
              shared: false,
            },
      // Both n1 and n2 fail their FIRST save_note_text call (a transient
      // storage error), then succeed on every subsequent call — independently
      // per note id, so each note's bounded retry must land on its own.
      save_note_text: (args: { id: string }) => {
        const w = window as unknown as { __attempts?: Record<string, number> };
        w.__attempts ??= {};
        w.__attempts[args.id] = (w.__attempts[args.id] ?? 0) + 1;
        if (w.__attempts[args.id] === 1) {
          return Promise.reject("storage error: database is locked");
        }
        return Date.now();
      },
    });

    await page.goto("/notes");
    await expect(page.locator(".notes-content")).toBeVisible();

    // Open BOTH notes as real tabs (drives TabsService.openNote for each,
    // registering them with TabRouteReuseStrategy).
    await page.locator(".title-btn", { hasText: "My First Note" }).click();
    await expect(page.locator(".note-title-input")).toHaveValue("My First Note");

    // In-app (client-side router) navigation back to the list — a hard
    // `page.goto` would force a full SPA reload and tear down the
    // already-opened "My First Note" tab, defeating the whole point of this
    // test (both notes must stay open as REAL, alive tabs simultaneously).
    await page.goBack();
    await expect(page.locator(".notes-content")).toBeVisible();
    await page.locator(".title-btn", { hasText: "Weekly plan" }).click();
    await expect(page.locator(".note-title-input")).toHaveValue("Weekly plan");

    // Two tabs are now open (n1 detached-but-alive, n2 active).
    const tabItems = page.locator("mur-tab-strip .tab-item");
    await expect(tabItems).toHaveCount(2);

    // Trigger Note B's (n2, currently active) autosave — it will fail once,
    // then schedule its bounded retry.
    const titleInput = page.locator(".note-title-input");
    await titleInput.fill("");
    await titleInput.type("Note B edited");

    // Switch back to Note A (n1) WHILE Note B's retry is still pending in its
    // 800ms backoff window, and edit it too — this is what used to let Note
    // B's `debounce.schedule` call under the SAME literal key cancel Note A's
    // still-pending retry timer.
    await tabItems.filter({ hasText: "My First Note" }).click();
    await expect(titleInput).toHaveValue("My First Note");
    await titleInput.fill("");
    await titleInput.type("Note A edited");

    // BOTH notes' autosave must independently recover to "saved" — neither
    // stuck on "saving" because the other note's retry cancelled it.
    const saveIndicator = page.locator(".save-state");
    await expect(saveIndicator).toHaveAttribute("data-state", "saved", {
      timeout: 5_000,
    });

    // Switch to Note B and confirm IT also reached "saved" (not wedged).
    await tabItems.filter({ hasText: /Note B edited|Weekly plan/ }).click();
    await expect(saveIndicator).toHaveAttribute("data-state", "saved", {
      timeout: 5_000,
    });

    expect(consoleErrors).toEqual([]);
  });
});
