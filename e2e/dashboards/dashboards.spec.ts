import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

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
  await mockTauri(page, {}, { list_dashboards: BOARDS, get_dashboard: BOARD_DETAIL });

  await page.goto("/dashboards");
  await expect(page.getByRole("heading", { name: "Dashboards", level: 1 })).toBeVisible();

  // Pinned first, and the card carries its source-mix chips.
  await expect(page.getByText("Atlas GA")).toBeVisible();
  await expect(page.getByText("Acme — the deal")).toBeVisible();
  await expect(page.getByText("1 recording")).toBeVisible();
  await expect(page.getByText("1 insight")).toBeVisible();

  // The miniature draws one box per tile — layout metadata, never a payload.
  await expect(page.locator(".board-card").first().locator(".mt")).toHaveCount(3);

  await page.getByRole("button", { name: "Open Atlas GA" }).click();
  await expect(page.getByRole("heading", { name: /Atlas GA/, level: 1 })).toBeVisible();
  await expect(page.getByText("Atlas GA checklist")).toBeVisible();
});

test("Dashboards: a SEALED tile leaks nothing — not even the title the user typed", async ({
  page,
}) => {
  await mockTauri(page, {}, { list_dashboards: BOARDS, get_dashboard: BOARD_DETAIL });

  await page.goto("/dashboards/b-atlas");
  await expect(page.getByText("Atlas GA checklist")).toBeVisible();

  const sealed = page.locator("app-dashboard-tile.is-locked");
  await expect(sealed).toHaveCount(1);
  // Copy tightened 2026-08-04 to tie the lock model to the board's thesis: a sealed
  // tile is not merely locked, it is OUT OF SCOPE for the board's Ask.
  await expect(sealed).toContainText("Sealed — not in scope");
  await expect(sealed.locator(".tile-title")).toHaveText("🔒 Locked");

  // The whole page must not contain the sealed title anywhere — heading, DOM
  // attribute, tooltip or otherwise.
  await expect(page.locator("body")).not.toContainText(SEALED_SECRET);
  const anywhere = await page.evaluate(
    (secret) => document.documentElement.outerHTML.includes(secret),
    SEALED_SECRET,
  );
  expect(anywhere, "the sealed title must not appear in the DOM at all").toBe(false);

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
  await expect(page.locator("app-dashboard-tile")).toHaveCount(1);
  await expect(page.locator("app-dashboard-tile .tile-title")).not.toContainText("Dana");
  // A drift lane with no rows never had data, so it collapses to a strip rather
  // than reserving a full card for an apology (2026-08-04 density pass). The
  // leak assertion above is the point of this test and is unchanged.
  await expect(page.locator("app-dashboard-tile")).toHaveClass(/is-empty/);
  await expect(page.getByText(/Values land here as they get revised/)).toBeVisible();
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
  await expect(page.getByText(/The saved answer is hidden/)).toBeVisible();
  await expect(page.getByText(/one of the sources it was built from is sealed/)).toBeVisible();
});

test("Dashboards: board Ask is scoped to the board's sources and cites the tiles it used", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      ask_vault: () => ({
        answer: "Jun 14 is at risk — the migration still has a sprint left.",
        sources: [{ meetingId: "m-1", title: "Atlas weekly", startedAt: "2026-06-03T09:00:00Z" }],
        citations: [],
      }),
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

  // The scope line is explicit about WHAT the answer may draw on, and says out
  // loud that the sealed tile is excluded.
  await expect(page.getByText(/2 readable sources/)).toBeVisible();
  await expect(page.getByText(/1 sealed tile is excluded until unlocked/)).toBeVisible();

  await page.getByRole("textbox", { name: "Ask a question about this board" }).fill("Will we make Jun 14?");
  await page.getByRole("button", { name: "Ask", exact: true }).click();

  await expect(page.getByText(/Jun 14 is at risk/)).toBeVisible();

  // The tile whose source grounded the answer lights up and numbers itself.
  const cited = page.locator("app-dashboard-tile.cited");
  await expect(cited).toHaveCount(1);
  await expect(cited.locator(".cite-n")).toHaveText("1");
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
  await expect(page.getByText("This board is empty.")).toBeVisible();
  await expect(page.getByRole("button", { name: "Add the first tile" })).toBeVisible();

  // Asking an empty board is honest rather than hallucinating from the vault.
  await page.getByRole("textbox", { name: "Ask a question about this board" }).fill("What is going on?");
  await page.getByRole("button", { name: "Ask", exact: true }).click();
  await expect(page.getByText(/no readable sources yet/)).toBeVisible();
});

test("Dashboards: the tile palette offers the catalogue and flags only-Murmur tiles", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-atlas");
  await page.getByRole("button", { name: "Add tile" }).click();

  const palette = page.getByRole("dialog", { name: "Add a tile" });
  await expect(palette).toBeVisible();
  await expect(palette.getByText("Drift lane")).toBeVisible();
  await expect(palette.getByText("Promise ledger")).toBeVisible();
  await expect(palette.getByText("Pulse")).toBeVisible();
  // The catalogue marks the tiles that only exist because Murmur heard the room.
  expect(await palette.locator(".only-badge").count()).toBeGreaterThanOrEqual(6);

  // Escape must always dismiss a modal.
  await page.keyboard.press("Escape");
  await expect(palette).toBeHidden();
});
