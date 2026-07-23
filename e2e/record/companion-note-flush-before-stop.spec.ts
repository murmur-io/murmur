import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * REGRESSION (root-cause fix, 2026-07-17) — FLUSH-BEFORE-FINALIZE for the recording
 * companion note.
 *
 * THE BUG: the recording panel's "Note" tab hosts the embedded note editor, which
 * persists via a 600ms-DEBOUNCED autosave. `RecorderStore.stop()` fired
 * `stop_recording` IMMEDIATELY, and `stop_recording` deletes the companion note if it
 * is still empty. So a user who typed in the Note tab and clicked Stop inside the
 * debounce window lost their prose: the debounced save hadn't landed, the DB body was
 * the empty eager-created stub, delete-if-empty fired, and the late save hit `no note`
 * — the text vanished from the note, the vault, AND the summary.
 *
 * The verifier repro was the INVOKE ORDER: `["stop_recording", "save_note_text:...the
 * text..."]` — Stop reached the backend BEFORE the text persisted.
 *
 * THE FIX: `RecorderStore.stop()` awaits the live companion editor's durable flush
 * (RecordingFlushService → NoteEditorComponent.flushPendingSave → the cheap text-save chain)
 * BEFORE `stop_recording`. This test proves the ORDER is now inverted: the companion
 * save (`save_note_text`, carrying the typed text) reaches the backend BEFORE
 * `stop_recording`.
 *
 * RED-before-GREEN: on the pre-fix code (no flush await, editor re-mounted per tab) the
 * companion save is the DEBOUNCED autosave, which only fires ~600ms later — well AFTER
 * the immediate `stop_recording` — so the assertion `updateIdx < stopIdx` FAILS. With
 * the fix it PASSES. (Verified RED first by running this spec against the working tree
 * with the `await this.flushService.flush()` line removed from `stop()` — the recorded
 * order was `[..., "stop_recording", "save_note_text"]`, `updateIdx > stopIdx`.)
 */
test.describe("Record — companion note is flushed BEFORE stop_recording", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({
        meetingId: "m-rec",
        startedAt: "2026-07-01T09:00:00Z",
      }),
      get_or_create_companion_note: () => ({
        noteId: "n1",
        meetingWikilink: "[[Test Meeting]]",
      }),
      // The eager companion note is EMPTY (only the managed front-matter link) — the
      // exact state that makes delete_companion_note_if_empty destructive if the
      // user's text hasn't been flushed yet.
      get_note: () => ({
        id: "n1",
        title: "Test Meeting",
        folderId: "",
        markdown: '---\nmeeting: "[[Test Meeting]]"\n---\n',
        tags: [],
        properties: {},
        updatedAt: 0,
        createdAt: 0,
        exportedPath: null,
        locked: false,
        shared: false,
      }),
      get_backlinks: () => [],
      // A full boundary save remains mocked, but Stop itself must use only the cheap
      // `save_note_text` path so re-index/export cannot delay capture finalization.
      update_note_doc: (args: any) => ({
        id: "n1",
        title: args.title,
        folderId: "",
        markdown: args.markdown,
        tags: [],
        properties: {},
        updatedAt: Date.now(),
        createdAt: 0,
        exportedPath: null,
        locked: false,
        shared: false,
      }),
      save_note_text: () => Date.now(),
      stop_recording: () => ({
        meetingId: "m-rec",
        markdown: "# Note\n",
        exportedPath: null,
      }),
    });

    // Record the ORDERED invoke stream page-side (AFTER the mock installs invoke, so
    // this wraps it). For the two writes we care about, capture the markdown too, so
    // we can assert the user's typed prose actually shipped — not an empty payload.
    await page.addInitScript(() => {
      (window as unknown as { __invokes: string[] }).__invokes = [];
      const internals = (
        window as unknown as {
          __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> };
        }
      ).__TAURI_INTERNALS__;
      const orig = internals.invoke.bind(internals);
      internals.invoke = (cmd: string, args: unknown) => {
        const rec = (window as unknown as { __invokes: string[] }).__invokes;
        if (cmd === "update_note_doc" || cmd === "save_note_text") {
          const md = (args as { markdown?: string } | undefined)?.markdown ?? "";
          rec.push(`${cmd}:${md}`);
        } else if (cmd === "stop_recording") {
          const completed = (
            args as { companionFlushCompleted?: boolean } | undefined
          )?.companionFlushCompleted;
          rec.push(`${cmd}:${String(completed)}`);
        } else {
          rec.push(cmd);
        }
        return orig(cmd, args);
      };
    });
  });

  test("typing then an immediate Stop persists the note before stop_recording", async ({
    page,
  }) => {
    await page.goto("/record");
    await page.locator("button.start-btn").click();
    await expect(page.locator("button.stop-btn")).toBeVisible({ timeout: 10_000 });

    // Type prose into the companion note body (Note tab is the default).
    const body = page.locator(
      "app-meeting-conversation app-note-editor .editor-body textarea.body-area",
    );
    await expect(body).toBeVisible({ timeout: 10_000 });
    await body.fill("critical decision: ship the migration on Friday");

    // Click Stop IMMEDIATELY — well inside the 600ms autosave debounce window. The
    // fix must flush the pending edit to the backend BEFORE stop_recording.
    await page.locator("button.stop-btn").click();

    // Wait until BOTH the flush write and stop_recording have reached the backend.
    await expect
      .poll(async () =>
        page.evaluate(() => {
          const inv = (window as unknown as { __invokes: string[] }).__invokes;
          const hasWrite = inv.some((c) => c.startsWith("save_note_text:"));
          const hasStop = inv.some((c) => c.startsWith("stop_recording:"));
          return hasWrite && hasStop;
        }),
      )
      .toBe(true);

    const order = await page.evaluate(
      () => (window as unknown as { __invokes: string[] }).__invokes,
    );

    // The companion save carrying the TYPED text must precede stop_recording.
    const writeIdx = order.findIndex(
      (c) => c.startsWith("save_note_text:") && c.includes("critical decision"),
    );
    const stopIdx = order.indexOf("stop_recording:true");

    expect(writeIdx, "companion save (with typed text) must be recorded").toBeGreaterThanOrEqual(0);
    expect(
      stopIdx,
      "stop_recording must carry the completed-flush cleanup witness",
    ).toBeGreaterThanOrEqual(0);
    expect(
      writeIdx,
      `companion save must reach the backend BEFORE stop_recording (order: ${JSON.stringify(order)})`,
    ).toBeLessThan(stopIdx);
  });
});
