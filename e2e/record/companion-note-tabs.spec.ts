import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * v2 DOCUMENT-FIRST recording panel — a two-tab surface (Note | Ask Brain).
 *
 *  - The "Note" tab (default) mounts the EMBEDDED note editor on the meeting's ONE
 *    companion note (eagerly created via `get_or_create_companion_note`). It is ONE
 *    editable DOCUMENT — no per-jot "Saved to Notes" badges.
 *  - The "Ask Brain" tab hosts the `@brain` conversation. A plain single-line input
 *    opens a thread; the answer renders with the brain identity.
 *  - The tabs switch cleanly (the segmented control drives `activeTab`).
 *
 * These replace the retired v1 specs (`companion-note-card` / `note-save-failure-toast`),
 * which asserted on the removed `mur-markdown-composer` + per-jot "Saved to Notes" card.
 */
test.describe("Record — document-first companion-note tabs (v2)", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({
        meetingId: "m-rec",
        startedAt: "2026-07-01T09:00:00Z",
      }),
      // Eager get-or-create for the "Note" tab's embedded editor mount.
      get_or_create_companion_note: () => ({
        noteId: "n1",
        meetingWikilink: "[[Test Meeting]]",
      }),
      // The embedded editor loads its body from get_note — an EMPTY companion note
      // (only the managed front-matter link, no user prose).
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
      ask_assistant_chat: (args: any) => ({
        intentKind: "recall",
        status: "ok",
        summary: "Atlas unblocked the mobile redesign for Q4.",
        command:
          args.messages && args.messages.length
            ? args.messages[args.messages.length - 1].text
            : "",
        citations: ["[[Q2 Roadmap Planning]]"],
        proposedNote: null,
        threadId: args.threadId || "t-live-1",
      }),
    });
  });

  async function startRecording(page: import("@playwright/test").Page) {
    await page.goto("/record");
    await page.locator("button.start-btn").click();
    await expect(page.locator("button.stop-btn")).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator("app-meeting-conversation")).toBeVisible();
  }

  test("(a) the Note tab shows an editable document body — no per-jot badges", async ({
    page,
  }) => {
    await startRecording(page);

    // The default tab is "Note": the embedded editor mounts on the companion note.
    const body = page.locator(
      "app-meeting-conversation app-note-editor .editor-body textarea.body-area",
    );
    await expect(body).toBeVisible({ timeout: 10_000 });
    await expect(body).toBeEditable();

    // It is the DOCUMENT — none of the retired v1 per-jot surfaces exist.
    await expect(page.locator("mur-markdown-composer")).toHaveCount(0);
    await expect(page.locator(".saved-open")).toHaveCount(0);
    await expect(page.locator(".meeting-chip")).toHaveCount(0);

    // The user can type into the document.
    await body.fill("ship the three flows people actually use");
    await expect(body).toHaveValue(/ship the three flows/);
  });

  test("(b) switching to Ask Brain shows the conversation and can ask", async ({
    page,
  }) => {
    await startRecording(page);

    // Switch to the Ask Brain tab via the segmented control.
    await page
      .locator("app-meeting-conversation mur-segmented")
      .getByText("Ask Brain", { exact: true })
      .click();

    const ask = page.locator("app-meeting-conversation .ask-input");
    await expect(ask).toBeVisible();

    // The embedded document editor stays MOUNTED for the whole recording (the fix
    // keeps ONE live editor to flush at Stop) but is HIDDEN while on the Ask tab —
    // so it must be present-but-not-visible, not removed from the DOM.
    await expect(
      page.locator("app-meeting-conversation app-note-editor"),
    ).toBeHidden();

    // Ask a question → a thread opens and the brain answers.
    await ask.fill("why was the mobile redesign deferred?");
    await page.locator("app-meeting-conversation .send-btn").click();

    await expect(page.locator("app-note-item")).toHaveCount(1);
    await expect(
      page.getByText("Atlas unblocked the mobile redesign for Q4."),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("(c) tabs switch cleanly back and forth", async ({ page }) => {
    await startRecording(page);

    const seg = page.locator("app-meeting-conversation mur-segmented");
    const body = page.locator(
      "app-meeting-conversation app-note-editor .editor-body textarea.body-area",
    );

    // Note tab (default) → editor mounted.
    await expect(body).toBeVisible({ timeout: 10_000 });

    // → Ask Brain: editor HIDDEN (stays mounted — one live flush target), ask input shown.
    await seg.getByText("Ask Brain", { exact: true }).click();
    await expect(page.locator("app-meeting-conversation .ask-input")).toBeVisible();
    await expect(page.locator("app-meeting-conversation app-note-editor")).toBeHidden();

    // → back to Note: the editor re-shows + reloads in place (no re-mount), ask input gone.
    await seg.getByText("Note", { exact: true }).click();
    await expect(body).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("app-meeting-conversation .ask-input")).toHaveCount(0);
  });
});
