import { test, expect, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

type SaveTextHandler = (args: any) => unknown;
const pageErrors = new WeakMap<Page, string[]>();

async function bootWithFlushProvider(
  page: Page,
  saveNoteText: SaveTextHandler,
): Promise<void> {
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({
      meetingId: "m-rec",
      startedAt: "2026-07-22T09:00:00Z",
    }),
    get_or_create_companion_note: () => ({
      noteId: "n1",
      meetingWikilink: "[[Test Meeting]]",
    }),
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
    save_note_text: saveNoteText,
    stop_recording: (args: unknown) => {
      const state = window as unknown as {
        __stopArgs?: Array<{ companionFlushCompleted?: boolean }>;
      };
      state.__stopArgs ??= [];
      state.__stopArgs.push(
        (args ?? {}) as { companionFlushCompleted?: boolean },
      );
      return {
        meetingId: "m-rec",
        markdown: "# Note\n",
        exportedPath: null,
      };
    },
  });
  await page.addInitScript(() => {
    (
      window as unknown as {
        __stopArgs: Array<{ companionFlushCompleted?: boolean }>;
        __updateAttempts: number;
      }
    ).__stopArgs = [];
    (window as unknown as { __updateAttempts: number }).__updateAttempts = 0;
  });
  await page.goto("/record");
  await page.locator("button.start-btn").click();
  await expect(page.locator("button.stop-btn")).toBeVisible({ timeout: 10_000 });
}

async function typeCompanionText(page: Page): Promise<void> {
  const body = page.locator(
    "app-meeting-conversation app-note-editor .editor-body textarea.body-area",
  );
  await expect(body).toBeVisible({ timeout: 10_000 });
  await body.fill("late companion save must survive Stop");
}

async function stopArgs(
  page: Page,
): Promise<Array<{ companionFlushCompleted?: boolean }>> {
  return page.evaluate(
    () =>
      (
        window as unknown as {
          __stopArgs: Array<{ companionFlushCompleted?: boolean }>;
        }
      ).__stopArgs,
  );
}

async function updateAttempts(page: Page): Promise<number> {
  return page.evaluate(
    () => (window as unknown as { __updateAttempts: number }).__updateAttempts,
  );
}

test.describe("Record — flush deadline preserves companion data", () => {
  test.beforeEach(async ({ page }) => {
    const errors: string[] = [];
    pageErrors.set(page, errors);
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(message.text());
    });
    page.on("pageerror", (error) => errors.push(String(error)));
  });

  test.afterEach(async ({ page }) => {
    expect(pageErrors.get(page) ?? []).toEqual([]);
  });

  test("a never-resolving flush invokes Stop once and forbids empty-stub deletion", async ({
    page,
  }) => {
    await bootWithFlushProvider(page, () => new Promise(() => {}));
    await typeCompanionText(page);
    await page.locator("button.stop-btn").click();

    await expect.poll(async () => (await stopArgs(page)).length).toBe(1);
    expect((await stopArgs(page))[0]?.companionFlushCompleted).toBe(false);
    await page.waitForTimeout(250);
    expect(await stopArgs(page)).toHaveLength(1);
  });

  test("a save slower than the deadline lands late while Stop preserves its row", async ({
    page,
  }) => {
    await bootWithFlushProvider(
      page,
      () =>
        new Promise((resolve) => {
          setTimeout(() => {
            (
              window as unknown as { __slowCompanionSaveLanded: boolean }
            ).__slowCompanionSaveLanded = true;
            resolve(Date.now());
          }, 2_500);
        }),
    );
    await typeCompanionText(page);
    await page.locator("button.stop-btn").click();

    await expect.poll(async () => (await stopArgs(page)).length).toBe(1);
    expect((await stopArgs(page))[0]?.companionFlushCompleted).toBe(false);
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __slowCompanionSaveLanded?: boolean })
              .__slowCompanionSaveLanded ?? false,
        ),
      )
      .toBe(true);
    expect(await stopArgs(page)).toHaveLength(1);
  });

  test("a rejected save and rejected retry forbid empty-stub deletion", async ({
    page,
  }) => {
    await bootWithFlushProvider(page, () => {
      const state = window as unknown as { __updateAttempts: number };
      state.__updateAttempts += 1;
      return Promise.reject(new Error("temporary storage failure"));
    });
    await typeCompanionText(page);
    await page.locator("button.stop-btn").click();

    await expect.poll(async () => (await stopArgs(page)).length).toBe(1);
    expect(await updateAttempts(page)).toBe(2);
    expect((await stopArgs(page))[0]?.companionFlushCompleted).toBe(false);
  });

  test("an unretryable save rejection forbids empty-stub deletion", async ({
    page,
  }) => {
    await bootWithFlushProvider(page, () => {
      const state = window as unknown as { __updateAttempts: number };
      state.__updateAttempts += 1;
      return Promise.reject(new Error("Locked: companion note is sealed"));
    });
    await typeCompanionText(page);
    await page.locator("button.stop-btn").click();

    await expect.poll(async () => (await stopArgs(page)).length).toBe(1);
    expect(await updateAttempts(page)).toBe(1);
    expect((await stopArgs(page))[0]?.companionFlushCompleted).toBe(false);
  });

  test("a completed empty flush explicitly allows empty-stub cleanup", async ({
    page,
  }) => {
    await bootWithFlushProvider(page, () => Date.now());
    await expect(
      page.locator(
        "app-meeting-conversation app-note-editor .editor-body textarea.body-area",
      ),
    ).toBeVisible({ timeout: 10_000 });
    await page.locator("button.stop-btn").click();

    await expect.poll(async () => (await stopArgs(page)).length).toBe(1);
    expect((await stopArgs(page))[0]?.companionFlushCompleted).toBe(true);
  });
});
