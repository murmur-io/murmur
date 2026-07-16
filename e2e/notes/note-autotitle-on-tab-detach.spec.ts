import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Root-cause fix (2026-07-15): `NoteEditorComponent.maybeAutoTitle()` was
 * correctly written (no-ops on a real title / an empty body, else calls
 * `suggest_note_title` and updates the tab label) but its ONLY trigger was
 * `DestroyRef.onDestroy(...)` — and `notes/:id` tabs are DETACHED, not
 * destroyed, on a plain tab switch (`TabRouteReuseStrategy.shouldDetach`
 * returns `true` for this route, per its own header doc). So the
 * overwhelmingly common real-world flow — type a note, click a different
 * open tab — never ran auto-title; a user had no reason to expect they'd
 * need to literally close the tab (✕) for a title to appear.
 *
 * The fix subscribes to a NEW `TabRouteReuseStrategy.onDetach(...)` — the one
 * place that genuinely knows "this tab is being backgrounded right now" (its
 * `store()`, called by the router mid-navigation) — and runs the same
 * best-effort `maybeAutoTitle()` (+ the `dirtyFull` full-save flush) at that
 * earlier moment too, filtered to the detaching tab's OWN key. The existing
 * `onDestroy` trigger stays for a real hard-close / app-quit.
 *
 * RED contract: reverting the `tabRouteReuse.onDetach(...)` subscription
 * (back to the `onDestroy`-only trigger) reproduces `suggest_note_title`
 * NEVER firing on a plain tab switch — confirmed locally against the
 * pre-fix code before landing this test.
 */
test.describe("Note editor — auto-title fires on tab BACKGROUND (detach), not only hard-close", () => {
  test("typing a note then switching tabs (without closing) auto-titles it", async ({
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
      // n1 = genuinely "Untitled" with an empty body (so opening it never
      // itself satisfies maybeAutoTitle's guards); n2 is the second open tab
      // switched to and back from. Both must be DISTINCT ids for the
      // TabRouteReuseStrategy detach/attach cache to exercise two real slots.
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
              title: "Untitled",
              folderId: "nf1",
              markdown: "",
              tags: [],
              properties: {},
              updatedAt: 1_720_000_000_000,
              createdAt: 1_719_000_000_000,
              exportedPath: null,
              locked: false,
              shared: false,
            },
      list_notes: () => [
        {
          id: "n1",
          title: "Untitled",
          folderId: "nf1",
          snippet: "",
          tags: [],
          updatedAt: 1_720_000_000_000,
          createdAt: 1_719_000_000_000,
          locked: false,
          shared: false,
        },
        {
          id: "n2",
          title: "Weekly plan",
          folderId: "nf1",
          snippet: "Ship the notes feature",
          tags: [],
          updatedAt: 1_720_100_000_000,
          createdAt: 1_719_100_000_000,
          locked: false,
          shared: true,
        },
      ],
      // Record every suggest_note_title call (page-side array; the override
      // runs page-side, no closures over test-scope per mockTauri's contract).
      suggest_note_title: (args: { noteId: string }) => {
        const w = window as unknown as { __titleCalls?: string[] };
        w.__titleCalls ??= [];
        w.__titleCalls.push(args.noteId);
        return "Generated title";
      },
    });

    await page.goto("/notes");
    await expect(page.locator(".notes-content")).toBeVisible();

    // Open the "Untitled" note as a real tab (drives TabsService.openNote,
    // registering it with TabRouteReuseStrategy).
    await page.locator(".title-btn", { hasText: "Untitled" }).click();
    const bodyArea = page.locator(".body-area");
    await expect(bodyArea).toBeVisible();

    // Type real body content — the note is still titled "Untitled".
    await bodyArea.fill("Meeting notes: discussed the roadmap for Q3.");
    await expect(page.locator(".note-title-input")).toHaveValue("Untitled");

    // RED checkpoint: switching tabs alone must not (pre-fix) have called
    // suggest_note_title yet — confirms this test actually exercises the
    // detach path, not some other trigger firing coincidentally.
    const callsBeforeSwitch = await page.evaluate(
      () => (window as unknown as { __titleCalls?: string[] }).__titleCalls ?? [],
    );
    expect(callsBeforeSwitch).toEqual([]);

    // Open a SECOND note as a different tab — in-app (client-side router) back
    // to the list, then click the other row, exactly like the existing
    // cross-tab specs (a hard `page.goto` would tear down the first tab
    // instead of merely detaching it). This detaches (not destroys) the
    // "Untitled" note's editor — NOT closing the first tab.
    await page.goBack();
    await expect(page.locator(".notes-content")).toBeVisible();
    await page.locator(".title-btn", { hasText: "Weekly plan" }).click();
    await expect(page.locator(".note-title-input")).toHaveValue("Weekly plan");

    // The mocked suggest_note_title must have fired for n1 as a DIRECT
    // consequence of the tab switch (detach), with no need to close the tab.
    await expect(async () => {
      const calls = await page.evaluate(
        () => (window as unknown as { __titleCalls?: string[] }).__titleCalls ?? [],
      );
      expect(calls).toContain("n1");
    }).toPass({ timeout: 2_000 });

    // The tab-strip label for the FIRST (backgrounded) tab updates in place —
    // TabsService.setTitle, driven by maybeAutoTitle's `.then` — with no need
    // to revisit that tab first. Confirms the tab strip visibly reflects the
    // auto-title, not just that the backend call fired.
    const tabItems = page.locator("mur-tab-strip .tab-item");
    await expect(tabItems.filter({ hasText: "Generated title" })).toBeVisible({
      timeout: 2_000,
    });

    // Switching back to it shows the SAME note (still "Untitled" server-side —
    // the mock's get_note is unchanged — but the open tab now carries the
    // generated label).
    await tabItems.filter({ hasText: "Generated title" }).click();
    await expect(page.locator(".note-title-input")).toHaveValue("Untitled");

    expect(consoleErrors).toEqual([]);
  });
});
