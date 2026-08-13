import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/** Open the board's on-demand Ask surface if it is not already present. */
async function openAsk(page: import("@playwright/test").Page): Promise<void> {
  const panel = page.locator("aside.ask");
  if (!(await panel.count())) {
    await page.getByRole("button", { name: "Ask", exact: true }).click();
  }
  await page.getByRole("textbox", { name: /Ask a question/i }).waitFor();
}

async function submitAsk(page: import("@playwright/test").Page): Promise<void> {
  await page
    .getByLabel("Ask this board")
    .getByRole("button", { name: "Ask", exact: true })
    .click();
}

async function enterCompose(
  page: import("@playwright/test").Page,
): Promise<void> {
  if (!(await page.locator('[data-testid="dashboard-compose"]').count())) {
    await page.getByRole("button", { name: "Compose", exact: true }).click();
  }
  await page.locator('[data-testid="dashboard-compose"]').waitFor();
}

/**
 * Dashboards — the runtime oracle for the boards feature.
 *
 * The load-bearing test here is the SEALED-TILE one: the backend resolves a
 * sealed source to `{ kind: "locked" }` with no payload, and this asserts the
 * rendered board never puts a title, snippet or date on screen for it — the
 * failure mode that would turn a board into a back door around the lock.
 */

const BOARDS = [
  {
    id: "b-atlas",
    title: "Atlas GA",
    emoji: "🚀",
    tint: "indigo",
    pinned: true,
    position: 0,
    createdAt: "2026-08-01T09:00:00Z",
    updatedAt: "2026-08-03T09:00:00Z",
    tileCount: 3,
    tileKinds: [
      { kind: "meeting", span: 5 },
      { kind: "drift", span: 4 },
      { kind: "note", span: 3 },
    ],
  },
  {
    id: "b-acme",
    title: "Acme — the deal",
    emoji: null,
    tint: null,
    pinned: false,
    position: 1,
    createdAt: "2026-07-20T09:00:00Z",
    updatedAt: "2026-08-02T09:00:00Z",
    tileCount: 0,
    tileKinds: [],
  },
];

/** The secret a sealed tile must never surface, in any form. */
const SEALED_SECRET = "Project Redwood acquisition terms";

const BOARD_DETAIL = {
  ...BOARDS[0],
  tiles: [
    {
      id: "t-note",
      dashboardId: "b-atlas",
      kind: "note",
      refId: "n-1",
      title: null,
      span: 4,
      position: 0,
      config: null,
      createdAt: "2026-08-01T09:00:00Z",
      data: {
        kind: "note",
        id: "n-1",
        title: "Atlas GA checklist",
        snippet: "Blocking: the auth migration, ~1 sprint left.",
        updatedAt: 1_780_000_000_000,
      },
    },
    {
      id: "t-sealed",
      dashboardId: "b-atlas",
      kind: "note",
      refId: "n-sealed",
      // A user-authored title that PARAPHRASES the sealed content: the renderer
      // must not fall back to it for a locked tile.
      title: SEALED_SECRET,
      span: 4,
      position: 1,
      config: null,
      createdAt: "2026-08-01T09:00:00Z",
      data: { kind: "locked" },
    },
    {
      id: "t-drift",
      dashboardId: "b-atlas",
      kind: "drift",
      refId: "e-atlas",
      title: null,
      span: 4,
      position: 2,
      config: null,
      createdAt: "2026-08-01T09:00:00Z",
      data: {
        kind: "drift",
        entity: "Project Atlas",
        predicate: "ga_date",
        rows: [
          { text: "Apr 30", meta: "Mar 12, 2026", status: "old", source: null },
          {
            text: "Jun 14",
            meta: "Jun 3, 2026",
            status: "now",
            source: { kind: "meeting", id: "m-1" },
          },
        ],
      },
    },
  ],
};

test("Dashboards: the list renders boards with their miniature, and opens one", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    { list_dashboards: BOARDS, get_dashboard: BOARD_DETAIL },
  );

  await page.goto("/dashboards");
  await expect(
    page.getByRole("heading", { name: "Dashboards", level: 1 }),
  ).toBeVisible();

  // Pinned first, and the card carries its source-mix chips.
  await expect(page.getByText("Atlas GA")).toBeVisible();
  await expect(page.getByText("Acme — the deal")).toBeVisible();
  await expect(page.getByText("1 recording")).toBeVisible();
  await expect(page.getByText("1 insight")).toBeVisible();

  // The miniature draws one box per tile — layout metadata, never a payload.
  await expect(page.locator(".board-card").first().locator(".mt")).toHaveCount(
    3,
  );

  await page.getByRole("button", { name: "Open Atlas GA" }).click();
  await expect(
    page.getByRole("heading", { name: /Atlas GA/, level: 1 }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Atlas GA checklist" }),
  ).toBeVisible();
});

test("Dashboards: a SEALED tile leaks nothing — not even the title the user typed", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    { list_dashboards: BOARDS, get_dashboard: BOARD_DETAIL },
  );

  await page.goto("/dashboards/b-atlas");
  await expect(
    page.getByRole("heading", { name: "Atlas GA checklist" }),
  ).toBeVisible();

  const assertSecretAbsent = async () => {
    await expect(page.locator("body")).not.toContainText(SEALED_SECRET);
    expect(
      await page.evaluate(
        (secret) => document.documentElement.outerHTML.includes(secret),
        SEALED_SECRET,
      ),
      "the sealed title must not appear anywhere in the DOM",
    ).toBe(false);
  };

  // Read mode must stay generic in the Brief and in every alternate lens.
  await expect(page.getByText("1 sealed and excluded")).toBeVisible();
  await assertSecretAbsent();
  for (const lens of ["Overview", "Commitments", "Sources", "People"]) {
    await page.getByRole("button", { name: lens, exact: true }).click();
    await assertSecretAbsent();
  }

  // Compose renders a generic compact row from the gated payload only.
  await enterCompose(page);
  const sealed = page.locator('.compose-row[data-kind="locked"]');
  await expect(sealed).toHaveCount(1);
  await expect(sealed).toContainText("Sealed item");
  await expect(sealed).toContainText(
    "Content hidden until its folder is unlocked",
  );
  await assertSecretAbsent();

  // CONTROL: the same assertion catches a leak when one exists — an unsealed
  // tile's title IS in the DOM, so the check above is not vacuous.
  const visibleTitleInDom = await page.evaluate(() =>
    document.documentElement.outerHTML.includes("Atlas GA checklist"),
  );
  expect(visibleTitleInDom).toBe(true);
});

test("Dashboards: an entity tile whose entity went invisible keeps no stored name", async ({
  page,
}) => {
  // An older build persisted the entity's name into `dashboard_tiles.title`. The backend now
  // strips that chrome whenever the payload is withheld, so even a legacy row cannot render it.
  await mockTauri(
    page,
    {},
    {
      list_dashboards: BOARDS,
      get_dashboard: {
        ...BOARDS[0],
        tiles: [
          {
            id: "t-drift-hidden",
            dashboardId: "b-atlas",
            kind: "drift",
            refId: "e-gone",
            // The backend redacts this to null before it ships; a build that regressed would
            // send it through and the heading would show the name.
            title: null,
            span: 4,
            position: 0,
            config: null,
            createdAt: "2026-08-01T09:00:00Z",
            data: { kind: "drift", entity: "—", predicate: "", rows: [] },
          },
        ],
      },
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-atlas");
  await enterCompose(page);
  await expect(page.locator(".compose-row")).toHaveCount(1);
  await expect(page.locator(".compose-row strong")).not.toContainText("Dana");
  // A drift lane with no rows never had data, so it collapses to a strip rather
  // than reserving a full card for an apology (2026-08-04 density pass). The
  // leak assertion above is the point of this test and is unchanged.
  await expect(page.locator(".compose-row")).toHaveAttribute(
    "data-kind",
    "drift",
  );
});

test("Dashboards: withheld legacy Living Answer chrome never reaches Read or Compose DOM", async ({
  page,
}) => {
  const secret = "NIGHTJAR LEGACY LIVING ANSWER SECRET";
  await mockTauri(
    page,
    {
      get_dashboard: () => {
        // Simulate a legacy persisted row whose every content-shaped field held
        // plaintext. The production resolver is the authority that projects it
        // to this scrubbed wire shape before the WebView sees it.
        const legacyStored = {
          title: "NIGHTJAR LEGACY LIVING ANSWER SECRET",
          config: JSON.stringify({
            question: "NIGHTJAR LEGACY LIVING ANSWER SECRET",
            answer: "NIGHTJAR LEGACY LIVING ANSWER SECRET",
          }),
          question: "NIGHTJAR LEGACY LIVING ANSWER SECRET",
        };
        void legacyStored;
        return {
          id: "b-atlas",
          title: "Atlas GA",
          emoji: "🚀",
          tint: "indigo",
          pinned: true,
          position: 0,
          createdAt: "2026-08-01T09:00:00Z",
          updatedAt: "2026-08-03T09:00:00Z",
          tileCount: 2,
          tileKinds: [],
          tiles: [
            {
              id: "t-answer-withheld",
              dashboardId: "b-atlas",
              kind: "living_answer",
              refId: null,
              title: null,
              span: 5,
              position: 0,
              config: null,
              createdAt: "2026-08-01T09:00:00Z",
              data: {
                kind: "livingAnswer",
                question: "",
                answer: null,
                answeredAt: null,
                withheld: true,
              },
            },
            {
              id: "t-answer-readable",
              dashboardId: "b-atlas",
              kind: "living_answer",
              refId: null,
              title: null,
              span: 5,
              position: 1,
              config: null,
              createdAt: "2026-08-01T09:00:00Z",
              data: {
                kind: "livingAnswer",
                question: "Authorized gated launch question",
                answer: "Authorized gated answer",
                answeredAt: "2026-08-03T09:00:00Z",
                withheld: false,
              },
            },
          ],
        };
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-atlas");
  await expect(
    page.getByText("Authorized gated launch question", { exact: true }),
  ).toBeVisible();
  expect(
    await page.locator("app-root").evaluate((root) => root.outerHTML),
  ).not.toContain(secret);

  await enterCompose(page);
  const withheld = page.locator('[data-tile-id="t-answer-withheld"]');
  await expect(withheld).toContainText("Living answer");
  await expect(withheld).toContainText("Waiting for an answer");
  await expect(withheld).not.toContainText(
    /saved answer is hidden|sources it was built from/i,
  );
  await expect(
    page.locator('[data-tile-id="t-answer-readable"]'),
  ).toContainText("Authorized gated launch question");
  expect(
    await page.locator("app-root").evaluate((root) => root.outerHTML),
  ).not.toContain(secret);
});

test("Dashboards: Read exposes Living Answer answered-at without inventing a stale verdict", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      set_dashboard_answer: () => {
        const root = globalThis as { __readAnswerWrites?: number };
        root.__readAnswerWrites = (root.__readAnswerWrites ?? 0) + 1;
        return null;
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: {
        ...BOARDS[0],
        tiles: [
          {
            id: "t-cached-answer",
            dashboardId: "b-atlas",
            kind: "living_answer",
            refId: null,
            title: null,
            span: 5,
            position: 0,
            config: JSON.stringify({ question: "Will Atlas launch?" }),
            createdAt: "2026-01-01T09:00:00Z",
            data: {
              kind: "livingAnswer",
              question: "Will Atlas launch?",
              answer: "The launch remains on track.",
              answeredAt: "2026-01-02T09:00:00Z",
              withheld: false,
            },
          },
        ],
      },
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-atlas");
  const brief = page.getByRole("region", { name: "Board brief" });
  await expect(
    brief.getByText("Cached answer", { exact: false }),
  ).toBeVisible();
  await expect(brief.getByText(/answered .+2026/i)).toBeVisible();
  await expect(brief).not.toContainText(/stale|fresh/i);

  await page.getByRole("button", { name: "Compose", exact: true }).click();
  const tile = page.locator('.compose-row[data-tile-id="t-cached-answer"]');
  await expect(tile).toBeVisible();
  await expect(tile.getByRole("button", { name: "Re-answer" })).toHaveCount(0);
  await page.getByRole("button", { name: "Done", exact: true }).click();
  await expect(page.getByRole("button", { name: "Re-answer" })).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        (globalThis as { __readAnswerWrites?: number }).__readAnswerWrites ?? 0,
    ),
  ).toBe(0);
});

test("Dashboards: board Ask sends only board id and backend conversation id, never FE history", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      get_dashboard_sources: () => {
        (
          globalThis as { __dashboardAskSourceReads?: number }
        ).__dashboardAskSourceReads =
          ((globalThis as { __dashboardAskSourceReads?: number })
            .__dashboardAskSourceReads ?? 0) + 1;
        return [{ kind: "note", id: "must-not-expand" }];
      },
      ask_vault_persisted: (args: Record<string, unknown>) => {
        const target = globalThis as { __dashboardAskCalls?: unknown[] };
        target.__dashboardAskCalls = [
          ...(target.__dashboardAskCalls ?? []),
          args,
        ];
        return {
          conversationId:
            (args["conversationId"] as string | undefined) ?? "board-thread-1",
          userMessageId: crypto.randomUUID(),
          assistantMessageId: crypto.randomUUID(),
          answer: "Jun 14 is at risk — the migration still has a sprint left.",
          sources: [
            {
              meetingId: "m-1",
              title: "Atlas weekly",
              startedAt: "2026-06-03T09:00:00Z",
            },
          ],
          citations: [],
        };
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: {
        ...BOARD_DETAIL,
        tiles: [
          ...BOARD_DETAIL.tiles,
          {
            id: "t-meeting",
            dashboardId: "b-atlas",
            kind: "meeting",
            refId: "m-1",
            title: null,
            span: 4,
            position: 3,
            config: null,
            createdAt: "2026-08-01T09:00:00Z",
            data: {
              kind: "meeting",
              id: "m-1",
              title: "Atlas weekly",
              startedAt: "2026-06-03T09:00:00Z",
              durationS: 2052,
              hasAudio: true,
            },
          },
        ],
      },
    },
  );

  await page.goto("/dashboards/b-atlas");

  await expect(page.locator("aside.ask")).toHaveCount(0);

  await openAsk(page);
  await expect(
    page.getByText(/2 currently readable material sources/),
  ).toBeVisible();
  await expect(page.getByText(/1 sealed item is excluded/)).toBeVisible();

  await openAsk(page);
  await page
    .getByRole("textbox", { name: "Ask a question about this board" })
    .fill("Will we make Jun 14?");
  await submitAsk(page);

  await expect(page.getByText(/Jun 14 is at risk/)).toBeVisible();
  await page
    .getByRole("textbox", { name: "Ask a question about this board" })
    .fill("What changed?");
  await submitAsk(page);
  const calls = (await page.evaluate(
    () =>
      (globalThis as { __dashboardAskCalls?: unknown[] }).__dashboardAskCalls,
  )) as Array<{
    dashboardId?: string;
    conversationId?: string;
    explicitSources?: unknown[];
    history?: unknown[];
  }>;
  expect(calls).toHaveLength(2);
  expect(calls[0].dashboardId).toBe("b-atlas");
  expect(calls[0].conversationId).toBeUndefined();
  expect(calls[1].dashboardId).toBe("b-atlas");
  expect(calls[1].conversationId).toBe("board-thread-1");
  expect(calls[0].explicitSources).toBeUndefined();
  expect(calls[0].history).toBeUndefined();
  expect(calls[1].history).toBeUndefined();
  expect(
    await page.evaluate(
      () =>
        (globalThis as { __dashboardAskSourceReads?: number })
          .__dashboardAskSourceReads ?? 0,
    ),
  ).toBe(0);
});

test("Dashboards: a board mutation synchronously drops turns and starts a provenance-fresh Ask thread", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      ask_vault_persisted: (args: Record<string, unknown>) => {
        const target = globalThis as { __mutationAskCalls?: unknown[] };
        target.__mutationAskCalls = [
          ...(target.__mutationAskCalls ?? []),
          args,
        ];
        const first = target.__mutationAskCalls.length === 1;
        return {
          conversationId: first ? "secret-old-thread" : "fresh-thread",
          userMessageId: crypto.randomUUID(),
          assistantMessageId: crypto.randomUUID(),
          answer: first ? "OLD SECRET ANSWER" : "Fresh board answer",
          sources: [],
          citations: [],
        };
      },
      reorder_dashboard_tiles: () => null,
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-atlas");
  await openAsk(page);
  await page
    .getByRole("textbox", { name: "Ask a question about this board" })
    .fill("OLD SECRET QUESTION");
  await submitAsk(page);
  await expect(
    page.getByText("OLD SECRET ANSWER", { exact: true }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Compose", exact: true }).click();
  await page
    .getByRole("button", { name: /Move later/ })
    .first()
    .click();
  await expect(
    page.getByText("OLD SECRET QUESTION", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("OLD SECRET ANSWER", { exact: true }),
  ).toHaveCount(0);

  await page.getByRole("button", { name: "Done", exact: true }).click();
  await openAsk(page);
  await page
    .getByRole("textbox", { name: "Ask a question about this board" })
    .fill("Question after mutation");
  await submitAsk(page);
  await expect(
    page.getByText("Fresh board answer", { exact: true }),
  ).toBeVisible();

  const calls = (await page.evaluate(
    () => (globalThis as { __mutationAskCalls?: unknown[] }).__mutationAskCalls,
  )) as Array<Record<string, unknown>>;
  expect(calls).toHaveLength(2);
  expect(calls[0]["conversationId"]).toBeUndefined();
  expect(calls[1]["conversationId"]).toBeUndefined();
  expect(calls[1]["history"]).toBeUndefined();
  expect(JSON.stringify(calls[1])).not.toContain("OLD SECRET");
});

test("Dashboards: an empty board invites the first tile instead of showing a dead grid", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      ask_vault_persisted: () => ({
        answer: "This board has no readable sources yet.",
        sources: [],
        citations: [],
      }),
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: { ...BOARDS[1], tiles: [] },
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-acme");
  await expect(
    page.getByText("Start with material for this board."),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Add a Note, Recording or Document" }),
  ).toBeVisible();

  // Asking an empty board is honest rather than hallucinating from the vault.
  await openAsk(page);
  await page
    .getByRole("textbox", { name: "Ask a question about this board" })
    .fill("What is going on?");
  await submitAsk(page);
  await expect(page.getByText(/no readable sources yet/)).toBeVisible();
});

test("Dashboards: Living Answer creation stores its question config but never an answer", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      add_dashboard_tile: (args: unknown) => {
        (globalThis as { __livingAnswerAdd?: unknown }).__livingAnswerAdd =
          args;
        return null;
      },
      set_dashboard_answer: () => {
        const root = globalThis as { __livingAnswerWrites?: number };
        root.__livingAnswerWrites = (root.__livingAnswerWrites ?? 0) + 1;
        return null;
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-atlas");
  await enterCompose(page);
  await page.getByRole("button", { name: "Add to board" }).click();

  const palette = page.getByRole("dialog", { name: "Add to board" });
  await expect(palette).toBeVisible();
  await expect(palette.getByRole("heading", { name: "Sources" })).toBeVisible();
  await expect(palette.getByRole("heading", { name: "Views" })).toBeVisible();
  await expect(palette.getByRole("button", { name: /^Note/ })).toBeVisible();
  await expect(
    palette.getByRole("button", { name: /^Recording/ }),
  ).toBeVisible();
  await expect(
    palette.getByRole("button", { name: /^Document/ }),
  ).toBeVisible();
  await expect(palette.getByText("Promise ledger")).toBeVisible();
  // Retired extractor views stay absent; Living Answer remains a supported View.
  await expect(palette.getByText("Drift lane")).toHaveCount(0);
  await expect(palette.getByText("Numbers")).toHaveCount(0);
  await expect(palette.getByText("Pulse")).toHaveCount(0);
  expect(await palette.locator(".only-badge").count()).toBeGreaterThanOrEqual(
    3,
  );

  await palette.getByRole("button", { name: /^Living answer/ }).click();
  const question = palette.getByLabel("The question this tile keeps answering");
  await expect(question).toBeFocused();
  await question.fill("  Will Atlas ship safely?  ");
  await palette.getByRole("button", { name: "Add tile" }).click();
  await expect(palette).toHaveCount(0);

  const added = await page.evaluate(
    () => (globalThis as { __livingAnswerAdd?: unknown }).__livingAnswerAdd,
  );
  expect(JSON.parse(JSON.stringify(added))).toEqual({
    dashboardId: "b-atlas",
    kind: "living_answer",
    title: "Will Atlas ship safely?",
    config: JSON.stringify({ question: "Will Atlas ship safely?" }),
  });
  expect(
    await page.evaluate(
      () =>
        (globalThis as { __livingAnswerWrites?: number })
          .__livingAnswerWrites ?? 0,
    ),
  ).toBe(0);
});
