import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Calm-Notepad recording surface (2026-07-19) — the companion note is the
 * always-visible, centered HERO; Ask Brain is a SUMMONED opaque panel (footer ✦),
 * not a beside-the-editor split.
 *
 *  - The embedded note editor mounts on the meeting's ONE companion note (eagerly
 *    created via `get_or_create_companion_note`) and is ALWAYS VISIBLE — one editable
 *    DOCUMENT, no per-jot "Saved to Notes" badges.
 *  - The footer "Ask" pill summons the Ask-Brain panel hosting the `@brain` thread
 *    (preset chips + text/voice). The editor stays mounted (the flush-at-Stop target)
 *    and visible beneath the sheet.
 */
test.describe("Record — companion note hero + summoned Ask Brain panel", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({
        meetingId: "m-rec",
        startedAt: "2026-07-01T09:00:00Z",
      }),
      // Eager get-or-create for the hero editor's mount.
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

  test("(a) the note document is the always-visible editable hero — no per-jot badges", async ({
    page,
  }) => {
    await startRecording(page);

    // The embedded editor mounts on the companion note as the visible hero.
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

  test("(b) summoning the Ask Brain panel shows the conversation and can ask (editor stays visible)", async ({
    page,
  }) => {
    await startRecording(page);

    // Summon the Ask Brain panel via the footer ✦ pill.
    await page.locator("app-record .ask-pill").click();

    const ask = page.locator("app-meeting-conversation .ask-panel .ask-input");
    await expect(ask).toBeVisible();

    // The embedded document editor is the always-visible HERO — the panel floats
    // OVER the notepad without unmounting it (the flush-at-Stop target).
    await expect(
      page.locator("app-meeting-conversation app-note-editor"),
    ).toBeVisible();

    // Ask a question → a thread opens and the brain answers.
    await ask.fill("why was the mobile redesign deferred?");
    await page.locator("app-meeting-conversation .ask-panel .send-btn").click();

    await expect(page.locator("app-note-item")).toHaveCount(1);
    await expect(
      page.getByText("Atlas unblocked the mobile redesign for Q4."),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("(c) the Ask Brain panel summons and dismisses cleanly", async ({
    page,
  }) => {
    await startRecording(page);

    const pill = page.locator("app-record .ask-pill");
    const body = page.locator(
      "app-meeting-conversation app-note-editor .editor-body textarea.body-area",
    );

    // Default: the editor is the visible hero, panel closed (no ask panel).
    await expect(body).toBeVisible({ timeout: 10_000 });
    await expect(
      page.locator("app-meeting-conversation .ask-panel"),
    ).toHaveCount(0);

    // Summon the panel → the ask input shows, the editor STAYS visible beneath it.
    await pill.click();
    await expect(
      page.locator("app-meeting-conversation .ask-panel .ask-input"),
    ).toBeVisible();
    await expect(body).toBeVisible();

    // Dismiss via the panel close × → the editor reloads in place, panel gone.
    await page.locator("app-meeting-conversation .ask-panel-close").click();
    await expect(
      page.locator("app-meeting-conversation .ask-panel"),
    ).toHaveCount(0);
    await expect(body).toBeVisible();
  });
});
