import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Source-scoped Brain (Brain v3 PR-3) — the Ask page threads the
 * `<mur-source-picker>` selection into `ask_vault` as `explicitSources`.
 *
 * The picker DEFAULTS to empty on the Ask page (whole-vault; the user opts in),
 * so this test opens the picker, picks one candidate, then asks — and asserts
 * `ask_vault` was called with the picked source as `[{kind, id}]`. A control
 * turn asked with NO selection carries NO `explicitSources` key (undefined ⇒
 * omitted ⇒ today's whole-vault behavior).
 */
test("Ask page threads the picked source into ask_vault's explicitSources (empty selection ⇒ omitted)", async ({
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
    list_meetings: () => [
      {
        id: "m9",
        startedAt: "2026-07-01T10:00:00Z",
        endedAt: null,
        title: "Planning sync",
        durationS: 0,
        audioPath: null,
        status: "EXPORTED",
      },
    ],
    list_notes: () => [],
    // The picker's candidate feed — one meeting the user can scope to.
    list_link_candidates: () => [
      { kind: "meeting", id: "m9", title: "Planning sync" },
    ],
    ask_vault_persisted: (args: {
      conversationId?: string;
      explicitSources?: unknown;
    }) => {
      const w = window as unknown as {
        __askCalls?: { explicitSources: unknown }[];
      };
      w.__askCalls = w.__askCalls ?? [];
      w.__askCalls.push({ explicitSources: args.explicitSources ?? null });
      return {
        conversationId: args.conversationId ?? "conversation-1",
        userMessageId: crypto.randomUUID(),
        assistantMessageId: crypto.randomUUID(),
        answer: "Answer.",
        sources: [],
        citations: [],
      };
    },
  });

  await page.goto("/ask");

  const input = page.locator(".ask-input");
  await expect(input).toBeVisible();

  // Control turn: NO source picked → ask_vault gets no explicitSources.
  await input.fill("Whole-vault question");
  await input.press("Enter");
  await expect(
    page.locator(".ask-row.is-assistant .ask-bubble").last(),
  ).toContainText("Answer.");

  // The control turn already settled one user row.
  await expect(page.locator(".ask-row.is-user")).toHaveCount(1);

  // Open the picker, pick the one candidate → a chip appears.
  await page.locator("mur-source-picker .sp-trigger").click();
  // The popover (`.sp-pop`/`.sp-scrim`/`.sp-row`) is TELEPORTED to <body>
  // (appTeleportToBody), so it is NOT under `mur-source-picker` — locate it at
  // page level. The trigger + selected chips stay in-tree.
  await page.locator(".sp-row").first().click();
  await expect(page.locator("mur-source-picker .sp-chip-title")).toHaveText([
    "Planning sync",
  ]);
  // Close the popover so the composer regains focus.
  await page.locator(".sp-scrim").click();
  await expect(page.locator(".sp-pop")).toHaveCount(0);

  // Scoped turn: the picked meeting rides ask_vault as explicitSources. Click
  // Send explicitly (deterministic) and wait for the SECOND user row to land.
  await input.fill("Scoped question");
  await page.locator(".ask-send").click();
  await expect(page.locator(".ask-row.is-user")).toHaveCount(2);
  await expect(page.locator(".ask-row.is-user").nth(1)).toContainText(
    "Scoped question",
  );

  const calls = await page.evaluate(
    () =>
      (window as unknown as {
        __askCalls?: { explicitSources: { kind: string; id: string }[] | null }[];
      }).__askCalls ?? [],
  );
  expect(calls.length).toBe(2);
  // First (control) call: no scope threaded.
  expect(calls[0].explicitSources).toBeNull();
  // Second (scoped) call: exactly the picked meeting as {kind, id}.
  expect((calls[1].explicitSources ?? []).map((s) => `${s.kind}:${s.id}`)).toEqual([
    "meeting:m9",
  ]);

  expect(consoleErrors).toEqual([]);
});
