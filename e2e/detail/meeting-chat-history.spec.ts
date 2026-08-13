import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

test("meeting Ask history stays scoped, resumable, and contained in the narrow drawer", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
  await mockTauri(page);

  await page.goto("/meeting/m-atlas-roadmap");
  await page.getByRole("button", { name: /Ask/ }).click();
  const chat = page.locator("app-meeting-chat");
  const history = chat.getByRole("button", { name: "Conversation history" });
  const fresh = chat.getByRole("button", { name: "New conversation" });
  await expect(history).toBeVisible();
  await expect(fresh).toBeVisible();

  await history.click();
  await expect(chat.getByText("No conversations yet")).toBeVisible();
  await history.click();

  const input = chat.locator(".chat-input");
  await input.fill("What did this meeting decide?");
  await input.press("Enter");
  await expect(chat.locator(".chat-row.is-assistant")).toHaveCount(1);

  await history.click();
  const row = chat.locator("mur-chat-history .history-row");
  await expect(row).toHaveCount(1);
  await row.click();
  await expect(chat.locator(".chat-row")).toHaveCount(2);

  const geometry = await page.evaluate(() => {
    const drawer = document.querySelector(".ask-drawer")!;
    const historyPanel = document.querySelector("mur-chat-history");
    const chatElement = document.querySelector("app-meeting-chat")!;
    return {
      drawerOverflow: drawer.scrollWidth > drawer.clientWidth,
      chatOverflow: chatElement.scrollWidth > chatElement.clientWidth,
      historyOverflow: historyPanel
        ? historyPanel.scrollWidth > historyPanel.clientWidth
        : false,
    };
  });
  expect(geometry).toEqual({
    drawerOverflow: false,
    chatOverflow: false,
    historyOverflow: false,
  });

  // A different meeting id is a different canonical namespace.
  await page.evaluate(() => {
    history.pushState({}, "", "/meeting/m-eng-sync");
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
  await expect(page).toHaveURL(/\/meeting\/m-eng-sync$/);
  const other = page.locator("app-meeting-chat");
  if (!(await other.isVisible())) {
    await page.getByRole("button", { name: /Ask/ }).click();
  }
  await other.getByRole("button", { name: "Conversation history" }).click();
  await expect(other.getByText("No conversations yet")).toBeVisible();

  expect(errors).toEqual([]);
});

test("a late meeting answer cannot append after canonical history invalidation", async ({
  page,
}) => {
  await mockTauri(page, {
    chat_meeting_persisted: () =>
      new Promise((resolve) => {
        (
          window as unknown as {
            __resolveHeldMeetingAsk?: () => void;
          }
        ).__resolveHeldMeetingAsk = () =>
          resolve({
            conversationId: "old-meeting-conversation",
            userMessageId: crypto.randomUUID(),
            assistantMessageId: crypto.randomUUID(),
            answer: "STALE answer from the old meeting",
            sources: [],
            citations: [],
          });
      }),
  });

  await page.goto("/meeting/m-atlas-roadmap");
  await page.getByRole("button", { name: /Ask/ }).click();
  await page.locator("app-meeting-chat .chat-input").fill("Held old question");
  await page.locator("app-meeting-chat .chat-input").press("Enter");
  await expect(page.locator("app-meeting-chat .chat-typing")).toBeVisible();

  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://ask-history-invalidated", null);
  });
  await page.evaluate(() => {
    (
      window as unknown as { __resolveHeldMeetingAsk?: () => void }
    ).__resolveHeldMeetingAsk?.();
  });
  await page.waitForTimeout(100);

  const current = page.locator("app-meeting-chat");
  await expect(current.locator(".chat-typing")).toHaveCount(0);
  await expect(
    current.getByText("Held old question", { exact: true }),
  ).toHaveCount(0);
  await expect(
    current.getByText("STALE answer from the old meeting", { exact: true }),
  ).toHaveCount(0);
});

test("meeting history restores the live dashboard DTO and changing it starts a fresh anchored thread", async ({
  page,
}) => {
  await mockTauri(page, {
    list_ask_conversations: () => [
      {
        id: "saved-meeting-thread",
        scope: { kind: "meeting", refId: "m-atlas-roadmap" },
        title: "Saved meeting question",
        createdAt: "2026-08-06T01:00:00Z",
        updatedAt: "2026-08-06T01:01:00Z",
        messageCount: 2,
      },
    ],
    load_ask_conversation: () => ({
      id: "saved-meeting-thread",
      scope: { kind: "meeting", refId: "m-atlas-roadmap" },
      title: "Saved meeting question",
      selectedSources: [
        {
          kind: "meeting",
          id: "m-atlas-roadmap",
          title: "Q2 Roadmap Planning",
        },
        { kind: "note", id: "n-saved", title: "Saved meeting source" },
      ],
      dashboard: {
        id: "dashboard-meeting-history",
        title: "Live meeting dashboard title",
        emoji: "📅",
      },
      messages: [
        {
          id: "00000000-0000-4000-8000-000000000501",
          ordinal: 0,
          role: "user",
          content: "Saved meeting question",
          sources: [],
          citations: [],
          createdAt: "2026-08-06T01:00:00Z",
        },
        {
          id: "00000000-0000-4000-8000-000000000502",
          ordinal: 1,
          role: "assistant",
          content: "Saved meeting answer",
          sources: [],
          citations: [],
          createdAt: "2026-08-06T01:01:00Z",
        },
      ],
      createdAt: "2026-08-06T01:00:00Z",
      updatedAt: "2026-08-06T01:01:00Z",
    }),
    chat_meeting_persisted: (args: unknown) => {
      (
        window as unknown as { __freshMeetingArgs?: unknown }
      ).__freshMeetingArgs = args;
      return {
        conversationId: "fresh-meeting-thread",
        userMessageId: crypto.randomUUID(),
        assistantMessageId: crypto.randomUUID(),
        answer: "Fresh meeting answer",
        sources: [],
        citations: [],
      };
    },
  });

  await page.goto("/meeting/m-atlas-roadmap");
  await page.getByRole("button", { name: /Ask/ }).click();
  const chat = page.locator("app-meeting-chat");
  await chat.getByRole("button", { name: "Conversation history" }).click();
  await chat.locator(".history-row").click();
  await expect(
    chat.locator('[data-testid="selected-dashboard-chip"]'),
  ).toContainText("Live meeting dashboard title");

  await chat
    .getByRole("button", {
      name: "Remove dashboard Live meeting dashboard title",
    })
    .click();
  await expect(chat.locator(".chat-row")).toHaveCount(0);
  await expect(
    chat.locator('[data-testid="selected-dashboard-chip"]'),
  ).toHaveCount(0);
  await expect(chat.locator(".sp-chip-title")).toHaveText([
    "Q2 Roadmap Planning",
    "Saved meeting source",
  ]);

  await chat.locator(".chat-input").fill("Fresh after dashboard removal");
  await chat.locator(".chat-input").press("Enter");
  await expect(
    chat.getByText("Fresh meeting answer", { exact: true }),
  ).toBeVisible();
  const args = await page.evaluate(
    () =>
      (
        window as unknown as {
          __freshMeetingArgs?: {
            conversationId?: string;
            meetingId?: string;
            dashboardId?: string;
            explicitSources?: Array<{ kind: string; id: string }>;
          };
        }
      ).__freshMeetingArgs,
  );
  expect(args?.conversationId).toBeUndefined();
  expect(args?.meetingId).toBe("m-atlas-roadmap");
  expect(args?.dashboardId).toBeUndefined();
  expect(
    (args?.explicitSources ?? []).map(({ kind, id }) => ({ kind, id })),
  ).toEqual([
    { kind: "meeting", id: "m-atlas-roadmap" },
    { kind: "note", id: "n-saved" },
  ]);
});
