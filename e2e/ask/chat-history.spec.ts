import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

test("vault Ask lists, resumes, continues, and starts durable conversations without duplicating rows", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));

  await mockTauri(page, {
    list_link_candidates: () => [
      { kind: "meeting", id: "m-atlas-roadmap", title: "Atlas roadmap" },
    ],
  });
  await page.goto("/ask");

  const history = page.getByRole("button", { name: "Conversation history" });
  const fresh = page.getByRole("button", { name: "New conversation" });
  await expect(history).toBeVisible();
  await expect(fresh).toBeVisible();

  await history.click();
  await expect(page.getByText("No conversations yet")).toBeVisible();
  await history.click();

  // Persist a non-default source selection and prove resume restores it.
  await page.locator("mur-source-picker .sp-trigger").click();
  await page.locator(".sp-row").first().click();
  const selectedTitle = await page
    .locator("mur-source-picker .sp-chip-title")
    .first()
    .textContent();
  await page.locator(".sp-scrim").click();

  // The WebView display title is not persistence authority. Corrupt only the
  // outbound IPC copy after the picker populated the mock backend's canonical
  // title registry; resume must hydrate the title from that registry, not echo
  // this forged client value.
  await page.evaluate(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (command: string, args?: unknown) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const invoke = internals.invoke.bind(internals);
    internals.invoke = (command: string, args?: unknown) => {
      if (
        command === "ask_vault_persisted" &&
        typeof args === "object" &&
        args !== null &&
        "explicitSources" in args &&
        Array.isArray(args.explicitSources)
      ) {
        const explicitSources = args.explicitSources.map((source) =>
          typeof source === "object" && source !== null
            ? { ...source, title: "FORGED FE TITLE" }
            : source,
        );
        return invoke(command, { ...args, explicitSources });
      }
      return invoke(command, args);
    };
  });

  const input = page.locator(".ask-input");
  await input.fill("What was the Atlas decision?");
  await input.press("Enter");
  await expect(page.locator(".ask-row.is-assistant")).toHaveCount(1);

  await history.click();
  const firstRow = page.locator("mur-chat-history .history-row");
  await expect(firstRow).toHaveCount(1);
  await expect(firstRow).toContainText("What was the Atlas decision?");
  await firstRow.click();
  await expect(page.locator(".ask-row")).toHaveCount(2);
  await expect(page.locator("mur-source-picker .sp-chip-title").first()).toHaveText(
    selectedTitle ?? "",
  );
  await expect(
    page.locator("mur-source-picker .sp-chip-title").first(),
  ).not.toHaveText("FORGED FE TITLE");

  // Continue the canonical id: history still has one row, not a duplicate.
  await input.fill("Who owns the follow-up?");
  await input.press("Enter");
  await expect(page.locator(".ask-row.is-assistant")).toHaveCount(2);
  await history.click();
  await expect(page.locator("mur-chat-history .history-row")).toHaveCount(1);

  await fresh.click();
  await expect(page.locator(".ask-row")).toHaveCount(0);
  await input.fill("Start a separate topic");
  await input.press("Enter");
  await history.click();
  await expect(page.locator("mur-chat-history .history-row")).toHaveCount(2);
  await expect(page.getByText("Start a separate topic", { exact: true })).toBeVisible();

  expect(errors).toEqual([]);
});

test("vault Ask history keeps the conversation during list failure and retries", async ({
  page,
}) => {
  await mockTauri(page, {
    list_ask_conversations: () => {
      const state = window as unknown as { __historyListCalls?: number };
      state.__historyListCalls = (state.__historyListCalls ?? 0) + 1;
      if (state.__historyListCalls === 1) {
        throw new Error("simulated history failure");
      }
      return [];
    },
  });
  await page.goto("/ask");

  const input = page.locator(".ask-input");
  await input.fill("Keep this visible conversation");
  await input.press("Enter");
  await expect(page.locator(".ask-row")).toHaveCount(2);

  await page.getByRole("button", { name: "Conversation history" }).click();
  await expect(page.getByRole("alert")).toContainText(
    "Couldn’t load conversation history",
  );
  await page.getByRole("button", { name: "Retry" }).click();
  await expect(page.getByText("No conversations yet")).toBeVisible();

  // Closing history reveals the untouched conversation underneath.
  await page.getByRole("button", { name: "Conversation history" }).click();
  await expect(page.getByText("Keep this visible conversation", { exact: true })).toBeVisible();
});

test("vault Ask exposes an explicit loading state while history is fetched", async ({
  page,
}) => {
  await mockTauri(page, {
    list_ask_conversations: () =>
      new Promise((resolve) => {
        (
          window as unknown as { __resolveHistoryList?: () => void }
        ).__resolveHistoryList = () => resolve([]);
      }),
  });
  await page.goto("/ask");

  await page.getByRole("button", { name: "Conversation history" }).click();
  await expect(page.getByText("Loading conversations…")).toBeVisible();
  await page.evaluate(() => {
    (
      window as unknown as { __resolveHistoryList?: () => void }
    ).__resolveHistoryList?.();
  });
  await expect(page.getByText("No conversations yet")).toBeVisible();
});

test("a rejected first send leaves no orphan durable-history row", async ({
  page,
}) => {
  await mockTauri(page, {
    ask_vault_persisted: () => {
      throw new Error("simulated first-send failure");
    },
  });
  await page.goto("/ask");

  await page.locator(".ask-input").fill("This must not become an orphan");
  await page.locator(".ask-input").press("Enter");
  await expect(page.getByRole("alert")).toBeVisible();

  await page.getByRole("button", { name: "Conversation history" }).click();
  await expect(page.locator("mur-chat-history .history-row")).toHaveCount(0);
  await expect(page.getByText("No conversations yet")).toBeVisible();
});

test("a failed row resume preserves the list and focus, then returns focus to the composer", async ({
  page,
}) => {
  await mockTauri(page, {
    list_ask_conversations: () => [
      {
        id: "focus-thread",
        scope: { kind: "vault" },
        title: "Focusable conversation",
        createdAt: "2026-08-06T01:00:00Z",
        updatedAt: "2026-08-06T01:01:00Z",
        messageCount: 2,
      },
    ],
    load_ask_conversation: () => {
      const state = window as unknown as { __focusLoadCalls?: number };
      state.__focusLoadCalls = (state.__focusLoadCalls ?? 0) + 1;
      if (state.__focusLoadCalls === 1) {
        throw new Error("simulated row failure");
      }
      return {
        id: "focus-thread",
        scope: { kind: "vault" },
        title: "Focusable conversation",
        selectedSources: [],
        messages: [
          {
            id: "00000000-0000-4000-8000-000000000101",
            ordinal: 0,
            role: "user",
            content: "Focusable question",
            sources: [],
            citations: [],
            createdAt: "2026-08-06T01:00:00Z",
          },
          {
            id: "00000000-0000-4000-8000-000000000102",
            ordinal: 1,
            role: "assistant",
            content: "Focusable answer",
            sources: [],
            citations: [],
            createdAt: "2026-08-06T01:01:00Z",
          },
        ],
        createdAt: "2026-08-06T01:00:00Z",
        updatedAt: "2026-08-06T01:01:00Z",
      };
    },
  });
  await page.goto("/ask");
  await page.getByRole("button", { name: "Conversation history" }).click();
  const row = page.getByRole("button", { name: /Focusable conversation/ });
  await row.focus();
  await row.press("Enter");

  await expect(page.getByRole("alert")).toContainText(
    "Couldn’t load this conversation",
  );
  await expect(row).toBeVisible();
  await expect(row).toBeFocused();

  await row.press("Enter");
  await expect(page.getByText("Focusable answer", { exact: true })).toBeVisible();
  await expect(page.locator(".ask-input")).toBeFocused();
});

test("a long vault history remains vertically reachable", async ({ page }) => {
  await mockTauri(page, {
    list_ask_conversations: () =>
      Array.from({ length: 50 }, (_, index) => ({
        id: `vault-long-thread-${index}`,
        scope: { kind: "vault" },
        title: `Vault conversation ${String(index + 1).padStart(2, "0")}`,
        createdAt: `2026-08-06T01:${String(index).padStart(2, "0")}:00Z`,
        updatedAt: `2026-08-06T01:${String(index).padStart(2, "0")}:30Z`,
        messageCount: 2,
      })),
  });
  await page.goto("/ask");
  await page.getByRole("button", { name: "Conversation history" }).click();

  const history = page.locator("mur-chat-history");
  const list = history.locator(".history-list");
  await expect(history.locator(".history-row")).toHaveCount(50);
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
    page.getByText("Vault conversation 50", { exact: true }),
  ).toBeVisible();
});
