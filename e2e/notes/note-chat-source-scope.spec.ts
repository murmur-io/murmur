import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Source-scoped Brain (Brain v3 PR-4) — smoke over the "Ask about this note"
 * panel, now hosted in the right-side "Ask Brain" DRAWER (re-homed 2026-07-17
 * from below the body, ROUTED mode). It:
 *   1. mounts inside the drawer for an OPEN note once the header toggle opens it,
 *   2. pre-fills its `<mur-source-picker>` with the note itself + its ACTIVE
 *      linked neighbours (via `list_links`), and
 *   3. sends `ask_vault` with a NON-empty `explicitSources` (the picked scope).
 *
 * The panel must be HIDDEN for a LOCKED note (the lock gate renders instead) —
 * never surface a note-chat behind a lock.
 *
 * `list_links` is mocked to return one active `note→meeting` edge so the default
 * scope is `[{note n1}, {meeting m9}]`; `ask_vault` records the `explicitSources`
 * it was called with on `window` so the test can assert the exact wire shape.
 */
test("note chat keeps its note anchor, adds one dashboard scope, and sends dashboardId without expanding it", async ({
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
    // One ACTIVE deterministic link from this note → a meeting, so the picker
    // pre-fills with the note + that neighbour. A `suggested` edge is included
    // to prove it is EXCLUDED from the default scope.
    list_links: (args: { kind: string; id: string }) => {
      if (args.kind === "note" && args.id === "n1") {
        return [
          {
            id: 1,
            direction: "out",
            otherKind: "meeting",
            otherId: "m9",
            otherTitle: "Planning sync",
            edgeType: "wikilink",
            createdBy: "auto",
            status: "active",
            score: 0,
            createdAt: 0,
          },
          {
            id: 2,
            direction: "out",
            otherKind: "note",
            otherId: "n-sugg",
            otherTitle: "A mere suggestion",
            edgeType: "semantic",
            createdBy: "auto",
            status: "suggested",
            score: 0.7,
            createdAt: 0,
          },
        ];
      }
      return [];
    },
    list_dashboards: () => [
      {
        id: "dashboard-note",
        title: "Note board live title",
        emoji: "📝",
        tileCount: 2,
        createdAt: "2026-08-06T01:00:00Z",
        updatedAt: "2026-08-06T01:00:00Z",
      },
    ],
    get_dashboard_sources: () => {
      (
        window as unknown as { __noteDashboardExpanded?: boolean }
      ).__noteDashboardExpanded = true;
      return [{ kind: "note", id: "must-not-expand" }];
    },
    ask_vault_persisted: (args: {
      explicitSources?: unknown;
      dashboardId?: string;
    }) => {
      (
        window as unknown as {
          __askVaultArgs?: unknown;
        }
      ).__askVaultArgs = args;
      return {
        conversationId: "note-conversation-1",
        userMessageId: crypto.randomUUID(),
        assistantMessageId: crypto.randomUUID(),
        answer: "Grounded answer.",
        sources: [],
        citations: [],
      };
    },
  });

  await page.goto("/notes/n1");
  await expect(page.locator(".note-title-input")).toHaveValue("My First Note");

  // The chat now lives in the right-side drawer — open it via the header toggle.
  await page.locator(".head-chat-btn").click();

  // The panel is mounted inside the open drawer.
  const chat = page.locator(".note-chat-drawer app-note-chat");
  await expect(chat).toBeVisible();
  await expect(chat.locator(".chat-title")).toHaveText("Ask about this note");

  // Pre-fill: the note itself + its ACTIVE linked meeting render as chips; the
  // `suggested` edge is excluded. Chip titles come from `SourceRef.title`.
  const chips = chat.locator(".sp-chip-title");
  await expect(chips).toHaveText(["My First Note", "Planning sync"]);

  await chat.locator("mur-source-picker .sp-trigger").click();
  await page
    .getByRole("option", { name: "Use dashboard Note board live title" })
    .click();
  await expect(
    chat.locator('[data-testid="selected-dashboard-chip"]'),
  ).toHaveCount(1);
  await expect(
    chat.locator('[data-testid="selected-dashboard-chip"]'),
  ).toContainText("Note board live title");

  // Ask a question — Enter sends via the composer.
  const input = chat.locator(".chat-input");
  await input.fill("Summarize this note");
  await input.press("Enter");
  await expect(
    chat.locator(".chat-row.is-assistant .chat-bubble").last(),
  ).toContainText("Grounded answer.");

  // ask_vault carried the pinned scope: exactly the note + its active link,
  // each as `{kind, id}` (title is display-only; the backend ignores it).
  const sent = await page.evaluate(() => {
    const target = window as unknown as {
      __askVaultArgs?: {
        explicitSources?: { kind: string; id: string }[];
        dashboardId?: string;
      };
      __noteDashboardExpanded?: boolean;
    };
    return {
      args: target.__askVaultArgs,
      expanded: target.__noteDashboardExpanded,
    };
  });
  expect(sent.args).toBeTruthy();
  const pairs = (sent.args?.explicitSources ?? []).map(
    (s) => `${s.kind}:${s.id}`,
  );
  expect(pairs).toEqual(["note:n1", "meeting:m9"]);
  expect(sent.args?.dashboardId).toBe("dashboard-note");
  expect(sent.expanded).not.toBe(true);

  expect(consoleErrors).toEqual([]);
});

/**
 * A LOCKED note renders the lock gate, and the Ask-about-this-note panel is
 * NEVER mounted (it lives inside the not-locked branch, `!embedded()`).
 */
test("Ask-about-this-note is hidden for a locked note (lock gate shown, no chat)", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page);
  await page.goto("/notes/nlk");

  // The lock gate is shown …
  await expect(page.locator(".lock-gate")).toBeVisible();
  // … and the note-chat panel is not mounted.
  await expect(page.locator("app-note-chat")).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});
