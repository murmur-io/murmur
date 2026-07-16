import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Regression for a HIGH-severity finding from adversarial review of PR #332
 * (which fixed only the save-RETRY key; this covers the other 7 call sites
 * that shared one bare literal `"note-editor-save"` key across every open
 * note tab): `DebounceService` is a `providedIn: 'root'` SINGLETON, and
 * `notes/:id` tabs stay ALIVE-BUT-DETACHED when backgrounded
 * (`TabRouteReuseStrategy`). `schedule(key, ...)` unconditionally
 * `clearTimeout`s any existing timer under the SAME key — so with two open
 * note tabs sharing a bare literal autosave key, typing in Note B while
 * Note A's autosave timer is still pending SILENTLY CANCELS Note A's timer
 * with no replacement ever scheduled for Note A. No error, no stuck
 * indicator — the edit in Note A simply never persists. Worse than the
 * retry-key bug (which at least left a visible "stuck on saving" symptom).
 *
 * The fix scopes every `debounce.schedule`/`cancel` call in the component to
 * `` `note-editor-save:${noteId}` `` (see `saveDebounceKey`), so each open
 * tab's autosave timer is independent in the shared singleton.
 *
 * ORIGINAL RED contract (still true in isolation — see `note-save-retry-
 * cross-tab.spec.ts` for the sibling retry-key collision, which remains
 * fully exercisable): reverting the scoping back to a bare literal key
 * reproduces Note A's edit never reaching `save_note_text` at all.
 *
 * STALE AS OF 2026-07-15 (auto-title-on-detach fix) — read before touching
 * this test: a note with unindexed edits (`dirtyFull`) now ALSO gets FULLY
 * flushed the instant its tab is backgrounded (`TabRouteReuseStrategy.
 * onDetach` → `runNoteBoundaryWork` → `flushFull`), not only on hard close.
 * That flush synchronously cancels the note's OWN pending debounce timer
 * and immediately queues a full save (`update_note_doc`) — which completes
 * (kicks off) before the user can possibly have switched tabs AND typed
 * into the next note, so Note A's timer is always gone by the time Note B
 * could ever call `schedule()` under a shared key. Verified empirically:
 * this test PASSES even with `saveDebounceKey` reverted to a bare literal —
 * the specific collision it was written to catch can no longer occur via
 * ordinary tab-switching, because the eager detach-flush structurally
 * prevents two notes' PRIMARY autosave timers from ever being pending
 * concurrently. This test is still a legitimate, valuable regression check
 * (both notes' edits must survive a rapid tab-switch, regardless of WHICH
 * save path lands them), but it no longer discriminates the debounce-key-
 * scoping fix specifically — do not read a pass here as proof that scoping
 * fix still works; that guarantee now lives structurally in the eager-flush
 * design instead. Tracks BOTH `save_note_text` and `update_note_doc` calls
 * since either is a genuine persist (the mock's default `update_note_doc`
 * echoes the real markdown back).
 */
test.describe("Note editor — concurrent open tabs never cancel each other's PENDING autosave", () => {
  test("editing note A then switching to note B before A's autosave fires still persists A's edit", async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        consoleErrors.push(msg.text());
      }
    });
    page.on("pageerror", (err) => consoleErrors.push(String(err)));

    const savedMarkdown: Record<string, string[]> = { n1: [], n2: [] };

    await mockNotes(page, {
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
      // Record every save (page-side array, read back via page.evaluate —
      // mockTauri overrides run page-side, no closures). Tracks BOTH the
      // cheap autosave (save_note_text) and the full save (update_note_doc)
      // — a note backgrounded with unindexed edits is now flushed via the
      // latter (see the class doc's 2026-07-15 note), so either path is a
      // legitimate persist of the edit under test.
      save_note_text: (args: { id: string; markdown: string }) => {
        const w = window as unknown as { __saves?: Record<string, string[]> };
        w.__saves ??= { n1: [], n2: [] };
        w.__saves[args.id] ??= [];
        w.__saves[args.id].push(args.markdown);
        return Date.now();
      },
      update_note_doc: (args: { id: string; title: string; markdown: string }) => {
        const w = window as unknown as { __saves?: Record<string, string[]> };
        w.__saves ??= { n1: [], n2: [] };
        w.__saves[args.id] ??= [];
        w.__saves[args.id].push(args.markdown);
        return {
          id: args.id,
          title: args.title,
          folderId: "nf1",
          markdown: args.markdown,
          tags: [],
          properties: {},
          updatedAt: Date.now(),
          createdAt: 1_719_000_000_000,
          exportedPath: null,
          locked: false,
          shared: false,
        };
      },
    });

    await page.goto("/notes");
    await expect(page.locator(".notes-content")).toBeVisible();

    // Open Note A and edit its BODY (drives onBodyInput → scheduleSave, a
    // 600ms-debounced autosave — the AUTOSAVE_MS constant in the component).
    await page.locator(".title-btn", { hasText: "My First Note" }).click();
    const bodyArea = page.locator(".body-area");
    await expect(bodyArea).toBeVisible();
    await bodyArea.fill("# Heading\n\nNote A's edit that must not be lost.");

    // IMMEDIATELY (well within the 600ms autosave window) switch to Note B
    // and edit it too — this is the exact interleaving that let Note B's
    // `debounce.schedule` call under an unscoped key silently cancel Note
    // A's still-pending autosave timer with nothing to replace it.
    await page.goBack();
    await expect(page.locator(".notes-content")).toBeVisible();
    await page.locator(".title-btn", { hasText: "Weekly plan" }).click();
    const bodyAreaB = page.locator(".body-area");
    await expect(bodyAreaB).toBeVisible();
    await bodyAreaB.fill("# Weekly plan\n\nNote B's edit.");

    // Give BOTH notes' independent 600ms autosave timers time to fire (a
    // generous margin over AUTOSAVE_MS=600ms).
    await page.waitForTimeout(1_200);

    const saves = await page.evaluate(
      () => (window as unknown as { __saves?: Record<string, string[]> }).__saves ?? {},
    );

    // Note B's edit must have persisted (this half always worked).
    expect(saves["n2"]?.some((md) => md.includes("Note B's edit"))).toBe(true);

    // Note A's edit must ALSO have independently persisted — this is the
    // fix under test. Pre-fix, Note A's autosave timer was cancelled by Note
    // B's `schedule` call under the same key, so `saves["n1"]` never
    // contains the edited text (silent loss, RED without the fix).
    expect(saves["n1"]?.some((md) => md.includes("Note A's edit that must not be lost"))).toBe(
      true,
    );

    expect(consoleErrors).toEqual([]);
  });
});
