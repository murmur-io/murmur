import { expect, test } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

test("authored-note Ask history survives drawer remount, resumes, and stays note-scoped", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
  await mockNotes(page);

  await page.goto("/notes/n1");
  await page.locator(".head-chat-btn").click();
  let drawer = page.locator(".note-chat-drawer");
  const history = drawer.getByRole("button", { name: "Conversation history" });
  const fresh = drawer.getByRole("button", { name: "New conversation" });
  await expect(history).toBeVisible();
  await expect(fresh).toBeVisible();

  await history.click();
  await expect(drawer.getByText("No conversations yet")).toBeVisible();
  await history.click();
  await drawer.locator(".chat-input").fill("What is missing from this note?");
  await drawer.locator(".chat-input").press("Enter");
  await expect(drawer.locator(".chat-row.is-assistant")).toHaveCount(1);

  // Destroy/recreate the drawer component, then recover from canonical history.
  await drawer.getByRole("button", { name: "Close Ask Brain" }).click();
  await page.locator(".head-chat-btn").click();
  drawer = page.locator(".note-chat-drawer");
  await drawer.getByRole("button", { name: "Conversation history" }).click();
  const row = drawer.locator("mur-chat-history .history-row");
  await expect(row).toHaveCount(1);
  await expect(row).toContainText("What is missing from this note?");

  const geometry = await page.evaluate(() => {
    const pane = document.querySelector(".note-chat-drawer")!;
    const panel = document.querySelector("mur-chat-history")!;
    return {
      paneOverflow: pane.scrollWidth > pane.clientWidth,
      panelOverflow: panel.scrollWidth > panel.clientWidth,
    };
  });
  expect(geometry).toEqual({ paneOverflow: false, panelOverflow: false });

  await row.click();
  await expect(drawer.locator(".chat-row")).toHaveCount(2);

  await page.evaluate(() => {
    history.pushState({}, "", "/notes/n2");
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
  await expect(page).toHaveURL(/\/notes\/n2$/);
  drawer = page.locator(".note-chat-drawer");
  await expect(drawer).toBeVisible();
  await drawer.getByRole("button", { name: "Conversation history" }).click();
  await expect(drawer.getByText("No conversations yet")).toBeVisible();

  expect(errors).toEqual([]);
});

test("a late default-source prefill cannot overwrite sources restored from history", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () =>
      new Promise((resolve) => {
        (
          window as unknown as { __resolveLateNoteLinks?: () => void }
        ).__resolveLateNoteLinks = () => resolve([]);
      }),
    list_ask_conversations: () => [
      {
        id: "saved-note-thread",
        scope: { kind: "note", refId: "n1" },
        title: "Saved question",
        createdAt: "2026-08-06T01:00:00Z",
        updatedAt: "2026-08-06T01:01:00Z",
        messageCount: 2,
      },
    ],
    load_ask_conversation: () => ({
      id: "saved-note-thread",
      scope: { kind: "note", refId: "n1" },
      title: "Saved question",
      selectedSources: [
        { kind: "note", id: "n-saved", title: "Saved source scope" },
      ],
      dashboard: {
        id: "dashboard-note-history",
        title: "Live note dashboard title",
        emoji: "📝",
      },
      messages: [
        {
          id: "00000000-0000-4000-8000-000000000401",
          ordinal: 0,
          role: "user",
          content: "Saved question",
          sources: [],
          citations: [],
          createdAt: "2026-08-06T01:00:00Z",
        },
        {
          id: "00000000-0000-4000-8000-000000000402",
          ordinal: 1,
          role: "assistant",
          content: "Saved answer",
          sources: [],
          citations: [],
          createdAt: "2026-08-06T01:01:00Z",
        },
      ],
      createdAt: "2026-08-06T01:00:00Z",
      updatedAt: "2026-08-06T01:01:00Z",
    }),
    ask_vault_persisted: (args: unknown) => {
      (window as unknown as { __freshNoteArgs?: unknown }).__freshNoteArgs =
        args;
      return {
        conversationId: "fresh-note-thread",
        userMessageId: crypto.randomUUID(),
        assistantMessageId: crypto.randomUUID(),
        answer: "Fresh note answer",
        sources: [],
        citations: [],
      };
    },
  });
  await page.goto("/notes/n1");
  await page.locator(".head-chat-btn").click();
  const drawer = page.locator(".note-chat-drawer");
  await drawer.getByRole("button", { name: "Conversation history" }).click();
  await drawer.locator(".history-row").click();
  const manualChips = drawer.locator(
    ".sp-chip:not(.sp-dashboard-chip) .sp-chip-title",
  );
  await expect(manualChips).toHaveText(["Saved source scope"]);
  await expect(
    drawer.locator('[data-testid="selected-dashboard-chip"]'),
  ).toContainText("Live note dashboard title");

  await drawer
    .getByRole("button", { name: "Remove dashboard Live note dashboard title" })
    .click();
  await expect(drawer.locator(".chat-row")).toHaveCount(0);
  await expect(
    drawer.locator('[data-testid="selected-dashboard-chip"]'),
  ).toHaveCount(0);
  await expect(manualChips).toHaveText(["Saved source scope"]);

  await drawer
    .locator(".chat-input")
    .fill("Fresh thread after dashboard removal");
  await drawer.locator(".chat-input").press("Enter");
  await expect(
    drawer.getByText("Fresh note answer", { exact: true }),
  ).toBeVisible();
  const freshArgs = await page.evaluate(
    () =>
      (
        window as unknown as {
          __freshNoteArgs?: {
            conversationId?: string;
            dashboardId?: string;
            explicitSources?: Array<{ kind: string; id: string }>;
          };
        }
      ).__freshNoteArgs,
  );
  expect(freshArgs?.conversationId).toBeUndefined();
  expect(freshArgs?.dashboardId).toBeUndefined();
  expect(
    (freshArgs?.explicitSources ?? []).map(({ kind, id }) => ({ kind, id })),
  ).toEqual([{ kind: "note", id: "n-saved" }]);

  await page.evaluate(() => {
    (
      window as unknown as { __resolveLateNoteLinks?: () => void }
    ).__resolveLateNoteLinks?.();
  });
  await expect(drawer.locator(".sp-chip-title")).toHaveText([
    "Saved source scope",
  ]);
});

test("a long authored-note history remains vertically reachable in the narrow drawer", async ({
  page,
}) => {
  await mockNotes(page, {
    list_ask_conversations: () =>
      Array.from({ length: 60 }, (_, index) => ({
        id: `long-thread-${index}`,
        scope: { kind: "note", refId: "n1" },
        title: `Conversation ${String(index + 1).padStart(2, "0")}`,
        createdAt: `2026-08-06T01:${String(index).padStart(2, "0")}:00Z`,
        updatedAt: `2026-08-06T01:${String(index).padStart(2, "0")}:30Z`,
        messageCount: 2,
      })),
  });

  await page.goto("/notes/n1");
  await page.locator(".head-chat-btn").click();
  const drawer = page.locator(".note-chat-drawer");
  await drawer.getByRole("button", { name: "Conversation history" }).click();

  const list = drawer.locator("mur-chat-history .history-list");
  await expect(drawer.locator(".history-row")).toHaveCount(60);
  const before = await list.evaluate((element) => ({
    clientHeight: element.clientHeight,
    scrollHeight: element.scrollHeight,
    scrollTop: element.scrollTop,
  }));
  expect(before.scrollHeight).toBeGreaterThan(before.clientHeight);
  expect(before.scrollTop).toBe(0);

  await list.hover();
  await page.mouse.wheel(0, 10_000);
  await expect
    .poll(() => list.evaluate((element) => element.scrollTop))
    .toBeGreaterThan(0);
  await expect(
    drawer.getByText("Conversation 60", { exact: true }),
  ).toBeVisible();
});

test("authored-note source titles wait for the privacy-listener barrier", async ({
  page,
}) => {
  const askEvent = "murmur://ask-history-invalidated";
  await mockNotes(
    page,
    {
      list_links: () => [
        {
          id: 93,
          direction: "out",
          otherKind: "meeting",
          otherId: "m-barrier-secret",
          otherTitle: "Barrier-protected source title",
          edgeType: "companion",
          createdBy: "auto",
          status: "active",
          score: 0,
          createdAt: 0,
        },
      ],
    },
    [askEvent],
  );
  await page.goto("/notes/n1");
  await page.locator(".head-chat-btn").click();

  const drawer = page.locator(".note-chat-drawer");
  await expect(drawer.locator(".chat-input")).toBeDisabled();
  await expect(drawer.locator(".sp-chip-title")).toHaveCount(0);
  await expect(
    drawer.getByText("Barrier-protected source title", { exact: true }),
  ).toHaveCount(0);

  await page.evaluate((event) => {
    const target = window as unknown as {
      __demoEmit: (name: string, payload: unknown) => void;
      __demoReleaseEventListeners: (name: string) => void;
    };
    target.__demoEmit(event, null);
    target.__demoReleaseEventListeners(event);
  }, askEvent);

  await expect(drawer.locator(".chat-input")).toBeEnabled();
  await expect(drawer.locator(".sp-chip-title")).toHaveText([
    "My First Note",
    "Barrier-protected source title",
  ]);
});
