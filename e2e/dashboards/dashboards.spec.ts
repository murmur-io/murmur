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
  if (
    !(await page.locator('section[aria-label="Compose board tiles"]').count())
  ) {
    await page.getByRole("button", { name: "Compose", exact: true }).click();
  }
  await page.locator('section[aria-label="Compose board tiles"]').waitFor();
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

  // Compose may render the legacy tile row, but only from its resolved locked
  // payload. Stored kind/title/config remain forbidden as fallback chrome.
  await enterCompose(page);
  const sealed = page.locator("app-dashboard-tile.is-locked");
  await expect(sealed).toHaveCount(1);
  await expect(sealed).toContainText("Sealed — not in scope");
  await expect(sealed.locator(".tile-title")).toHaveText("🔒 Locked");
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
  await expect(page.locator("app-dashboard-tile")).toHaveCount(1);
  await expect(
    page.locator("app-dashboard-tile .tile-title"),
  ).not.toContainText("Dana");
  // A drift lane with no rows never had data, so it collapses to a strip rather
  // than reserving a full card for an apology (2026-08-04 density pass). The
  // leak assertion above is the point of this test and is unchanged.
  await expect(page.locator("app-dashboard-tile")).toHaveClass(/is-empty/);
  await expect(
    page.getByText(/Values land here as they get revised/),
  ).toBeVisible();
});

test("Dashboards: a withheld Living answer shows why, and not the cached text", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      list_dashboards: BOARDS,
      get_dashboard: {
        ...BOARDS[0],
        tiles: [
          {
            id: "t-answer",
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
              question: "Will Acme renew?",
              answer: null,
              answeredAt: null,
              withheld: true,
            },
          },
        ],
      },
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-atlas");
  await enterCompose(page);
  await expect(page.getByText(/The saved answer is hidden/)).toBeVisible();
  await expect(
    page.getByText(/one of the sources it was built from is sealed/),
  ).toBeVisible();
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
  const tile = page.locator(
    'app-dashboard-tile[data-tile-id="t-cached-answer"]',
  );
  await expect(tile).toBeVisible();
  await expect(tile.getByRole("button", { name: "Re-answer" })).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (globalThis as { __readAnswerWrites?: number }).__readAnswerWrites ?? 0,
    ),
  ).toBe(0);
});

test("Dashboards: board Ask sends readable sources and the board id", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      ask_vault: (args: unknown) => {
        (globalThis as { __dashboardAskArgs?: unknown }).__dashboardAskArgs =
          args;
        return {
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
      get_dashboard_sources: [
        { kind: "meeting", id: "m-1" },
        { kind: "note", id: "n-1" },
      ],
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
  const sent = (await page.evaluate(
    () => (globalThis as { __dashboardAskArgs?: unknown }).__dashboardAskArgs,
  )) as { dashboardId?: string; explicitSources?: unknown[] };
  expect(sent.dashboardId).toBe("b-atlas");
  expect(sent.explicitSources).toEqual([
    { kind: "meeting", id: "m-1" },
    { kind: "note", id: "n-1" },
  ]);
});

test("Dashboards: an empty board invites the first tile instead of showing a dead grid", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
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
