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
test("meeting chat pre-fills this meeting + its links and sends chat_meeting with explicitSources", async ({
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
    chat_meeting: (args: { explicitSources?: unknown }) => {
      (window as unknown as { __chatSources?: unknown }).__chatSources =
        args.explicitSources ?? null;
      return "Grounded meeting answer.";
    },
  });

  await page.goto("/meeting/m-atlas-roadmap");

  // The meeting chat is on the default Note tab.
  const chat = page.locator("app-meeting-chat");
  await expect(chat).toBeVisible({ timeout: 10_000 });

  // Pre-fill: this meeting (by its TITLE, not id) + its active linked note.
  await expect(chat.locator(".sp-chip-title")).toHaveText([
    "Q2 Roadmap Planning",
    "Companion note",
  ]);

  // Ask a question — Enter sends.
  const input = chat.locator(".chat-input");
  await input.fill("What did we decide?");
  await input.press("Enter");
  await expect(
    chat.locator(".chat-row.is-assistant .chat-bubble").last(),
  ).toContainText("Grounded meeting answer.");

  // chat_meeting carried the pinned scope: the meeting + its active link.
  const sent = await page.evaluate(
    () =>
      (window as unknown as { __chatSources?: { kind: string; id: string }[] })
        .__chatSources,
  );
  expect(sent).toBeTruthy();
  expect((sent ?? []).map((s) => `${s.kind}:${s.id}`)).toEqual([
    "meeting:m-atlas-roadmap",
    "note:nX",
  ]);

  expect(consoleErrors).toEqual([]);
});
