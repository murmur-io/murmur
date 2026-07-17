import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * Source-scoped Brain (Brain v3 PR-4) — smoke over the "Ask about this note"
 * panel mounted below the note body (ROUTED mode). It:
 *   1. renders below the body for an OPEN note,
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
test("Ask-about-this-note renders below the body, pre-fills note + its links, and sends ask_vault with explicitSources", async ({
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
    ask_vault: (args: { explicitSources?: unknown }) => {
      (window as unknown as { __askVaultSources?: unknown }).__askVaultSources =
        args.explicitSources ?? null;
      return { answer: "Grounded answer.", sources: [], citations: [] };
    },
  });

  await page.goto("/notes/n1");

  // The panel is mounted below the body.
  const chat = page.locator("app-note-chat");
  await expect(chat).toBeVisible();
  await expect(chat.locator(".chat-title")).toHaveText("Ask about this note");

  // Pre-fill: the note itself + its ACTIVE linked meeting render as chips; the
  // `suggested` edge is excluded. Chip titles come from `SourceRef.title`.
  const chips = chat.locator(".sp-chip-title");
  await expect(chips).toHaveText(["My First Note", "Planning sync"]);

  // Ask a question — Enter sends via the composer.
  const input = chat.locator(".chat-input");
  await input.fill("Summarize this note");
  await input.press("Enter");
  await expect(
    chat.locator(".chat-row.is-assistant .chat-bubble").last(),
  ).toContainText("Grounded answer.");

  // ask_vault carried the pinned scope: exactly the note + its active link,
  // each as `{kind, id}` (title is display-only; the backend ignores it).
  const sent = await page.evaluate(
    () =>
      (window as unknown as { __askVaultSources?: { kind: string; id: string }[] })
        .__askVaultSources,
  );
  expect(sent).toBeTruthy();
  const pairs = (sent ?? []).map((s) => `${s.kind}:${s.id}`);
  expect(pairs).toEqual(["note:n1", "meeting:m9"]);

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
