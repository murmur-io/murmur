import { expect, test, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

const consoleErrors = new WeakMap<Page, string[]>();

test.beforeEach(({ page }) => {
  const errors: string[] = [];
  consoleErrors.set(page, errors);
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
});

test.afterEach(({ page }) => {
  expect(consoleErrors.get(page) ?? []).toEqual([]);
});

test("meeting commands sit between tags and tabs with padded icon actions and no duplicate reminder", async ({
  page,
}) => {
  await mockTauri(page, {}, { audit_reminder_suggestions: [] });
  await page.goto("/meeting/m-atlas-roadmap");

  const tags = page.locator(".tag-editor");
  const commands = page.getByTestId("meeting-command-bar");
  const tabs = page.locator("app-detail-tabs");
  await expect(commands).toBeVisible({ timeout: 10_000 });

  const [tagsBox, commandBox, tabsBox] = await Promise.all([
    tags.boundingBox(),
    commands.boundingBox(),
    tabs.boundingBox(),
  ]);
  expect(tagsBox).not.toBeNull();
  expect(commandBox).not.toBeNull();
  expect(tabsBox).not.toBeNull();
  expect(tagsBox!.y + tagsBox!.height).toBeLessThan(commandBox!.y);
  expect(commandBox!.y + commandBox!.height).toBeLessThan(tabsBox!.y);

  for (const name of [
    "New reminder",
    "Convert to note",
    "Re-summarize",
    "Edit",
    "More",
  ]) {
    const action = commands.getByRole("button", { name, exact: true });
    await expect(action).toBeVisible();
    await expect(action.locator("svg")).toHaveCount(1);
    const padding = await action.evaluate((element) => {
      const style = getComputedStyle(element);
      return [Number.parseFloat(style.paddingLeft), Number.parseFloat(style.paddingRight)];
    });
    expect(padding[0]).toBeGreaterThan(0);
    expect(padding[1]).toBeGreaterThan(0);
  }

  await expect(page.getByRole("button", { name: "New reminder" })).toHaveCount(1);
  await expect(page.locator("app-meeting-actions app-smart-reminder-card .smart-card")).toHaveCount(0);
});

test("meeting summary is one rich Markdown document", async ({ page }) => {
  const markdown = `# Strategy review

## Summary
A **clear decision** with [supporting context](https://example.com/context) and [[Roadmap follow-up]].

- Parent point
  - Nested detail

| Item | Owner |
| --- | --- |
| Launch | Ada |

> Keep the full context.`;
  await mockTauri(
    page,
    {},
    {
      audit_reminder_suggestions: [],
      get_meeting_detail: {
        meeting: {
          id: "m-atlas-roadmap",
          startedAt: "2026-07-20T09:00:00Z",
          endedAt: "2026-07-20T10:00:00Z",
          title: "Strategy review",
          durationS: 3600,
          audioPath: null,
          status: "SUMMARIZED",
          folderId: null,
        },
        note: {
          meetingId: "m-atlas-roadmap",
          providerId: "claude_code",
          markdown,
          exportedPath: null,
        },
        segments: [],
        assistantInteractions: [],
        aiProvider: "claude_code",
        aiModel: "gpt-5.6-codex",
        modelServed: "gpt-5.6-codex",
      },
    },
  );
  await page.goto("/meeting/m-atlas-roadmap");

  const note = page.locator("app-note-panel .meeting-markdown-note");
  await expect(note).toHaveCount(1);
  await expect(note.getByRole("heading", { name: "Summary" })).toBeVisible();
  await expect(note.locator("strong")).toHaveText("clear decision");
  await expect(note.locator('a[href="https://example.com/context"]')).toBeVisible();
  await expect(note.locator("ul ul")).toContainText("Nested detail");
  await expect(note.locator("table")).toContainText("Ada");
  await expect(note.locator("blockquote")).toContainText("full context");
  await expect(note.locator(".md-wikilink")).toHaveText("Roadmap follow-up");
});

test("Convert to note passes the selected template and opens a canonical note tab", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      convert_meeting_to_note: (args: unknown) => {
        const w = window as unknown as { __convertCalls?: unknown[] };
        (w.__convertCalls ??= []).push(args);
        return { noteId: "n-converted", meetingWikilink: "[[Strategy review]]" };
      },
    },
    {
      audit_reminder_suggestions: [],
      list_note_templates: [
        {
          id: "tpl-exec",
          name: "Executive brief",
          tone: "Direct",
          sections: [{ heading: "Decision", instruction: "State the decision." }],
          extraFrontmatterKeys: [],
          createdAt: "2026-08-13T09:00:00Z",
        },
      ],
    },
  );
  await page.goto("/meeting/m-atlas-roadmap");

  await page.getByRole("button", { name: "Choose note template" }).click();
  await page.getByRole("menuitem", { name: /Executive brief/ }).click();

  await expect(page).toHaveURL(/\/notes\/n-converted$/);
  const calls = await page.evaluate(
    () => (window as unknown as { __convertCalls?: unknown[] }).__convertCalls ?? [],
  );
  expect(calls).toEqual([
    { meetingId: "m-atlas-roadmap", templateId: "tpl-exec" },
  ]);
  const tabs = await page.evaluate(() =>
    JSON.parse(localStorage.getItem("murmur.tabs.v1") ?? "{}"),
  );
  expect(tabs.tabs).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ kind: "note", entityId: "n-converted" }),
    ]),
  );
});

test("a late conversion from a backgrounded meeting cannot disable or redirect the active meeting", async ({
  page,
}) => {
  await mockTauri(page, {
    get_meeting_detail: (args: unknown) => {
      const meetingId = (args as { meetingId: string }).meetingId;
      const suffix = meetingId === "m-convert-a" ? "A" : "B";
      return {
        locked: false,
        meeting: {
          id: meetingId,
          startedAt: "2026-08-13T09:00:00Z",
          endedAt: "2026-08-13T10:00:00Z",
          title: `Conversion ${suffix}`,
          durationS: 3600,
          audioPath: null,
          status: "SUMMARIZED",
          folderId: null,
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        aiProvider: "claude_code",
        aiModel: "gpt-5.6-codex",
        modelServed: "gpt-5.6-codex",
      };
    },
    convert_meeting_to_note: (args: unknown) => {
      const meetingId = (args as { meetingId: string }).meetingId;
      return new Promise((resolve) => {
        const w = window as unknown as {
          __resolveConversionA?: () => void;
          __resolveConversionB?: () => void;
        };
        const finish = () =>
          resolve({
            noteId: meetingId === "m-convert-a" ? "n-convert-a" : "n-convert-b",
            meetingWikilink:
              meetingId === "m-convert-a" ? "[[Conversion A]]" : "[[Conversion B]]",
          });
        if (meetingId === "m-convert-a") {
          w.__resolveConversionA = finish;
        } else {
          w.__resolveConversionB = finish;
        }
      });
    },
  });
  await page.goto("/meeting/m-convert-a");

  const convertA = page.getByRole("button", { name: "Convert to note", exact: true });
  await expect(convertA).toBeEnabled({ timeout: 10_000 });
  await convertA.click();
  await expect(page.getByRole("button", { name: "Converting…", exact: true })).toBeDisabled();

  // Client-side direct navigation preserves A as a detached live tab while B mounts.
  await page.evaluate(() => {
    window.history.pushState({}, "", "/meeting/m-convert-b");
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
  await expect(page).toHaveURL(/\/meeting\/m-convert-b$/);
  const convertB = page.getByRole("button", { name: "Convert to note", exact: true });
  await expect(convertB).toBeEnabled({ timeout: 10_000 });
  await convertB.click();
  const convertingB = page.getByRole("button", { name: "Converting…", exact: true });
  await expect(convertingB).toBeDisabled();

  // Settling A must neither navigate to A's note nor clear B's newer pending state.
  await page.evaluate(async () => {
    (window as unknown as { __resolveConversionA?: () => void }).__resolveConversionA?.();
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
  });
  await expect(page).toHaveURL(/\/meeting\/m-convert-b$/);
  await expect(convertingB).toBeDisabled();

  await page.evaluate(() =>
    (window as unknown as { __resolveConversionB?: () => void }).__resolveConversionB?.(),
  );
  await expect(page).toHaveURL(/\/notes\/n-convert-b$/);
});

test("a masked meeting cannot render Markdown, wikilinks, or attachments", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      get_meeting_detail: {
        locked: true,
        meeting: {
          id: "m-atlas-roadmap",
          startedAt: "2026-07-20T09:00:00Z",
          endedAt: null,
          title: "🔒 Locked",
          durationS: 0,
          audioPath: null,
          status: "SUMMARIZED",
          folderId: "f-sealed",
        },
        note: {
          meetingId: "m-atlas-roadmap",
          providerId: "claude_code",
          markdown: "# SEALED SUMMARY [[SEALED WIKILINK]] ![](murmur-attachment://00000000-0000-0000-0000-000000000001)",
          exportedPath: null,
        },
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      },
      list_note_attachments: [
        {
          id: "00000000-0000-0000-0000-000000000001",
          ownerKind: "meeting",
          ownerId: "m-atlas-roadmap",
          mime: "image/png",
          byteLen: 99,
          createdAt: "2026-08-13T09:00:00Z",
        },
      ],
    },
  );
  await page.goto("/meeting/m-atlas-roadmap");

  await expect(page.getByText("This meeting is locked")).toBeVisible();
  await expect(page.locator("app-note-panel")).toHaveCount(0);
  await expect(page.locator(".meeting-markdown-note, .md-wikilink, .md-attachment")).toHaveCount(0);
  await expect(page.getByText(/SEALED SUMMARY|SEALED WIKILINK/)).toHaveCount(0);
});

test("New reminder re-reads gated detail and uses no title when visibility changed", async ({
  page,
}) => {
  await mockTauri(page, {
    get_meeting_detail: () => {
      const w = window as unknown as { __meetingDetailReads?: number };
      w.__meetingDetailReads = (w.__meetingDetailReads ?? 0) + 1;
      if (w.__meetingDetailReads > 1) return null;
      return {
        locked: false,
        meeting: {
          id: "m-atlas-roadmap",
          startedAt: "2026-07-20T09:00:00Z",
          endedAt: "2026-07-20T10:00:00Z",
          title: "PRIVATE MEETING TITLE",
          durationS: 3600,
          audioPath: null,
          status: "SUMMARIZED",
          folderId: null,
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      };
    },
  });
  await page.goto("/meeting/m-atlas-roadmap");

  const reminder = page.getByRole("button", { name: "New reminder" });
  await expect(reminder).toBeEnabled({ timeout: 10_000 });
  await reminder.click();
  const composer = page.getByRole("dialog", { name: "New reminder" });
  await expect(composer).toBeVisible();
  await expect(composer.getByLabel("Title")).toHaveValue("");
  await expect(composer.getByText("PRIVATE MEETING TITLE")).toHaveCount(0);
});

test("a pending reminder title read cannot open the composer after navigation", async ({
  page,
}) => {
  await mockTauri(page, {
    get_meeting_detail: () => {
      const w = window as unknown as { __meetingDetailReads?: number };
      w.__meetingDetailReads = (w.__meetingDetailReads ?? 0) + 1;
      if (w.__meetingDetailReads > 1) {
        return new Promise((resolve) => {
          const state = window as unknown as { __resolveReminderDetail?: () => void };
          state.__resolveReminderDetail = () => resolve(null);
        });
      }
      return {
        locked: false,
        meeting: {
          id: "m-atlas-roadmap",
          startedAt: "2026-07-20T09:00:00Z",
          endedAt: null,
          title: "Private title",
          durationS: 60,
          audioPath: null,
          status: "SUMMARIZED",
          folderId: null,
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      };
    },
  });
  await page.goto("/meeting/m-atlas-roadmap");

  const reminder = page.getByRole("button", { name: "New reminder" });
  await expect(reminder).toBeEnabled({ timeout: 10_000 });
  await reminder.click();
  await page.goto("/library");
  await page.evaluate(() =>
    (window as unknown as { __resolveReminderDetail?: () => void }).__resolveReminderDetail?.(),
  );
  await expect(page.getByRole("dialog", { name: "New reminder" })).toHaveCount(0);
});
