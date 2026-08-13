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
      { kind: "note", id: "n-new", title: "Fresh manual source" },
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

  // Changing the manual scope starts a fresh durable thread.
  await input.fill("Scoped question");
  await page.locator(".ask-send").click();
  await expect(page.locator(".ask-row.is-user")).toHaveCount(1);
  await expect(page.locator(".ask-row.is-user").first()).toContainText(
    "Scoped question",
  );

  const calls = await page.evaluate(
    () =>
      (
        window as unknown as {
          __askCalls?: {
            explicitSources: { kind: string; id: string }[] | null;
          }[];
        }
      ).__askCalls ?? [],
  );
  expect(calls.length).toBe(2);
  // First (control) call: no scope threaded.
  expect(calls[0].explicitSources).toBeNull();
  // Second (scoped) call: exactly the picked meeting as {kind, id}.
  expect(
    (calls[1].explicitSources ?? []).map((s) => `${s.kind}:${s.id}`),
  ).toEqual(["meeting:m9"]);

  expect(consoleErrors).toEqual([]);
});

test("Ask selects one composite dashboard chip and sends its id beside manual sources", async ({
  page,
}) => {
  await mockTauri(page, {
    list_meetings: () => [
      {
        id: "m9",
        title: "Planning sync",
        startedAt: "2026-07-01T10:00:00Z",
        endedAt: null,
        durationS: 0,
        audioPath: null,
        status: "EXPORTED",
      },
      {
        id: "m10",
        title: "Fresh manual source",
        startedAt: "2026-07-02T10:00:00Z",
        endedAt: null,
        durationS: 0,
        audioPath: null,
        status: "EXPORTED",
      },
    ],
    list_notes: () => [],
    list_link_candidates: () => [
      { kind: "meeting", id: "m9", title: "Planning sync" },
      { kind: "meeting", id: "m10", title: "Fresh manual source" },
    ],
    list_dashboards: () => [
      {
        id: "dashboard-test",
        title: "Test",
        emoji: "🧭",
        tint: null,
        pinned: false,
        position: 0,
        createdAt: "2026-08-01T00:00:00Z",
        updatedAt: "2026-08-01T00:00:00Z",
        tileCount: 2,
        tileKinds: [],
      },
    ],
    get_dashboard_sources: () => {
      (
        window as unknown as { __dashboardChildrenRead?: number }
      ).__dashboardChildrenRead = 1;
      return [{ kind: "note", id: "child-must-not-leak" }];
    },
    ask_vault_persisted: (args: Record<string, unknown>) => {
      const target = window as unknown as { __compositeAskCalls?: unknown[] };
      target.__compositeAskCalls = [
        ...(target.__compositeAskCalls ?? []),
        args,
      ];
      return {
        conversationId: `composite-thread-${target.__compositeAskCalls.length}`,
        userMessageId: crypto.randomUUID(),
        assistantMessageId: crypto.randomUUID(),
        answer: "Composite answer",
        sources: [],
        citations: [],
      };
    },
  });
  await page.goto("/ask");
  const picker = page.locator("mur-source-picker");
  await picker.locator(".sp-trigger").click();
  await page.getByRole("option", { name: "Use dashboard Test" }).click();
  await expect(
    picker.locator('[data-testid="selected-dashboard-chip"]'),
  ).toHaveCount(1);
  await expect(
    picker.locator('[data-testid="selected-dashboard-chip"]'),
  ).toContainText("Test");

  await picker.locator(".sp-trigger").click();
  await page.getByRole("option", { name: "Planning sync" }).click();
  await page
    .locator(".sp-scrim")
    .evaluate((element) => (element as HTMLElement).click());
  await expect(page.locator(".sp-pop")).toHaveCount(0);
  await page.locator(".ask-input").fill("Use both scopes");
  await page.locator(".ask-input").press("Enter");
  await expect(
    page.getByText("Composite answer", { exact: true }),
  ).toBeVisible();

  await picker.getByRole("button", { name: "Remove Planning sync" }).click();
  await picker.locator(".sp-trigger").click();
  await page.getByRole("option", { name: "Fresh manual source" }).click();
  await page
    .locator(".sp-scrim")
    .evaluate((element) => (element as HTMLElement).click());
  await expect(page.locator(".sp-pop")).toHaveCount(0);
  await expect(
    picker.locator('[data-testid="selected-dashboard-chip"]'),
  ).toContainText("Test");
  await page.locator(".ask-input").fill("Use changed manual scope");
  await page.locator(".ask-input").press("Enter");
  await expect(page.locator(".ask-row.is-assistant")).toHaveCount(1);

  const state = await page.evaluate(() => ({
    calls:
      (window as unknown as { __compositeAskCalls?: Record<string, unknown>[] })
        .__compositeAskCalls ?? [],
    childReads:
      (window as unknown as { __dashboardChildrenRead?: number })
        .__dashboardChildrenRead ?? 0,
  }));
  expect(state.calls).toHaveLength(2);
  expect(state.childReads).toBe(0);
  expect(state.calls[0]?.["dashboardId"]).toBe("dashboard-test");
  expect(state.calls[0]?.["explicitSources"]).toEqual([
    { kind: "meeting", id: "m9", title: "Planning sync" },
  ]);
  expect(state.calls[1]?.["conversationId"]).toBeUndefined();
  expect(state.calls[1]?.["dashboardId"]).toBe("dashboard-test");
  expect(state.calls[1]?.["explicitSources"]).toEqual([
    { kind: "meeting", id: "m10", title: "Fresh manual source" },
  ]);
  expect(JSON.stringify(state.calls)).not.toContain("child-must-not-leak");
});
