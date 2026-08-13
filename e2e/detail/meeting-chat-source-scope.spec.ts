import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Source-scoped Brain (Brain v3 PR-3) — the meeting "Ask about this meeting"
 * chat pre-fills its `<mur-source-picker>` with THIS meeting + its ACTIVE linked
 * neighbours and threads that selection into `chat_meeting` as `explicitSources`.
 *
 * `list_links` is mocked to return one active `meeting→note` edge, so the
 * default scope is `[{meeting m-atlas-roadmap "Q2 Roadmap Planning"}, {note nX}]`;
 * `chat_meeting` records the `explicitSources` it saw on `window`.
 */
test("meeting chat keeps its meeting anchor, adds one dashboard scope, and sends dashboardId without expansion", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockTauri(page, {
    list_links: (args: { kind: string; id: string }) => {
      if (args.kind === "meeting") {
        return [
          {
            id: 1,
            direction: "out",
            otherKind: "note",
            otherId: "nX",
            otherTitle: "Companion note",
            edgeType: "companion",
            createdBy: "auto",
            status: "active",
            score: 0,
            createdAt: 0,
          },
        ];
      }
      return [];
    },
    list_dashboards: () => [
      {
        id: "dashboard-meeting",
        title: "Meeting board live title",
        emoji: "📅",
        tileCount: 2,
        createdAt: "2026-08-06T01:00:00Z",
        updatedAt: "2026-08-06T01:00:00Z",
      },
    ],
    get_dashboard_sources: () => {
      (
        window as unknown as { __meetingDashboardExpanded?: boolean }
      ).__meetingDashboardExpanded = true;
      return [{ kind: "meeting", id: "must-not-expand" }];
    },
    chat_meeting_persisted: (args: {
      explicitSources?: unknown;
      dashboardId?: string;
    }) => {
      (window as unknown as { __chatArgs?: unknown }).__chatArgs = args;
      return {
        conversationId: "meeting-conversation-1",
        userMessageId: crypto.randomUUID(),
        assistantMessageId: crypto.randomUUID(),
        answer: "Grounded meeting answer.",
        sources: [],
        citations: [],
      };
    },
  });

  await page.goto("/meeting/m-atlas-roadmap");

  // "Ask about this meeting" now lives in a summoned right-side drawer (default-
  // closed); open it via the header ✦ Ask button, then the chat is visible.
  await page.getByRole("button", { name: /Ask/ }).click();
  const chat = page.locator("app-meeting-chat");
  await expect(chat).toBeVisible({ timeout: 10_000 });

  // Regression: once the detail entrance animation completes, it must not retain
  // a transform. Otherwise it captures the fixed drawer and its bottom follows the
  // long document instead of the viewport.
  await expect
    .poll(() =>
      page
        .locator(".detail")
        .evaluate((element) => getComputedStyle(element).transform),
    )
    .toBe("none");
  const drawerBounds = await page.locator(".ask-drawer").boundingBox();
  const viewport = page.viewportSize();
  expect(drawerBounds).not.toBeNull();
  expect(viewport).not.toBeNull();
  expect(drawerBounds?.y).toBe(0);
  expect(drawerBounds?.height).toBe(viewport?.height);

  // Pre-fill: this meeting (by its TITLE, not id) + its active linked note.
  await expect(chat.locator(".sp-chip-title")).toHaveText([
    "Q2 Roadmap Planning",
    "Companion note",
  ]);

  await chat.locator("mur-source-picker .sp-trigger").click();
  await page
    .getByRole("option", { name: "Use dashboard Meeting board live title" })
    .click();
  await expect(
    chat.locator('[data-testid="selected-dashboard-chip"]'),
  ).toHaveCount(1);
  await expect(
    chat.locator('[data-testid="selected-dashboard-chip"]'),
  ).toContainText("Meeting board live title");

  // Ask a question — Enter sends.
  const input = chat.locator(".chat-input");
  await input.fill("What did we decide?");
  await input.press("Enter");
  await expect(
    chat.locator(".chat-row.is-assistant .chat-bubble").last(),
  ).toContainText("Grounded meeting answer.");

  // chat_meeting carried the pinned scope: the meeting + its active link.
  const sent = await page.evaluate(() => {
    const target = window as unknown as {
      __chatArgs?: {
        explicitSources?: { kind: string; id: string }[];
        dashboardId?: string;
      };
      __meetingDashboardExpanded?: boolean;
    };
    return {
      args: target.__chatArgs,
      expanded: target.__meetingDashboardExpanded,
    };
  });
  expect(sent.args).toBeTruthy();
  expect(
    (sent.args?.explicitSources ?? []).map((s) => `${s.kind}:${s.id}`),
  ).toEqual(["meeting:m-atlas-roadmap", "note:nX"]);
  expect(sent.args?.dashboardId).toBe("dashboard-meeting");
  expect(sent.expanded).not.toBe(true);

  expect(consoleErrors).toEqual([]);
});
