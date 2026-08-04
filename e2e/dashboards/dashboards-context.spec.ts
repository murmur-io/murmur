import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Dashboards as CONTEXT — the half that makes a board more than a view.
 *
 * A board is offered as a scope in the shared source picker, so every surface
 * that picks sources (Ask, note chat, reminders) can scope to one. Picking a
 * board expands it into the board's own VISIBLE sources — the backend drops
 * sealed ones, so this can never widen scope past what the session may read.
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
      { kind: "note", span: 4 },
      { kind: "drift", span: 3 },
    ],
  },
  {
    id: "b-empty",
    title: "Nothing on it yet",
    emoji: null,
    tint: null,
    pinned: false,
    position: 1,
    createdAt: "2026-08-01T09:00:00Z",
    updatedAt: "2026-08-01T09:00:00Z",
    tileCount: 0,
    tileKinds: [],
  },
];

test("Ask: a dashboard can be picked as scope and expands to its visible sources", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      list_dashboards: BOARDS,
      // Two visible sources; a sealed third is already filtered out backend-side.
      get_dashboard_sources: [
        { kind: "meeting", id: "m-1" },
        { kind: "note", id: "n-1" },
      ],
    },
  );

  await page.goto("/ask");
  await page.getByRole("button", { name: /Source/ }).first().click();

  // Boards are offered ABOVE notes/meetings — the user's own declaration of scope.
  const picker = page.locator(".sp-list");
  await expect(picker.getByText("Dashboards")).toBeVisible();
  await expect(picker.getByText("Atlas GA")).toBeVisible();
  // A board with no tiles is not a usable scope, so it is not offered.
  await expect(picker.getByText("Nothing on it yet")).toHaveCount(0);

  await picker.getByRole("option", { name: /Atlas GA/ }).click();

  // Picking the board added BOTH of its visible sources as chips.
  await expect(page.locator(".sp-chip, .sp .pill")).toHaveCount(2, { timeout: 5000 });
});

test("Dashboards: Arrange mode makes tiles draggable and persists a reorder", async ({
  page,
}) => {
  const tiles = [
    {
      id: "t-a", dashboardId: "b-atlas", kind: "note", refId: "n-1", title: null,
      span: 4, position: 0, config: null, createdAt: "2026-08-01T09:00:00Z",
      data: { kind: "note", id: "n-1", title: "FIRST TILE", snippet: "a", updatedAt: 1 },
    },
    {
      id: "t-b", dashboardId: "b-atlas", kind: "note", refId: "n-2", title: null,
      span: 4, position: 1, config: null, createdAt: "2026-08-01T09:00:00Z",
      data: { kind: "note", id: "n-2", title: "SECOND TILE", snippet: "b", updatedAt: 1 },
    },
  ];

  await mockTauri(
    page,
    {
      reorder_dashboard_tiles: (args: { tileIds?: string[] }) => {
        (window as unknown as { __reorder?: string[] }).__reorder = args?.tileIds;
        return null;
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: { ...BOARDS[0], tiles },
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-atlas");
  await expect(page.getByText("FIRST TILE")).toBeVisible();

  // Not draggable until Arrange mode is on — a board is read-first.
  await expect(page.locator("app-dashboard-tile").first()).not.toHaveAttribute(
    "draggable",
    "true",
  );

  await page.getByRole("button", { name: "Arrange" }).click();
  await expect(page.getByText(/drag tiles to reorder/)).toBeVisible();
  await expect(page.locator("app-dashboard-tile").first()).toHaveAttribute(
    "draggable",
    "true",
  );

  // Drag the second tile onto the first.
  const source = page.locator("app-dashboard-tile").nth(1);
  const target = page.locator("app-dashboard-tile").nth(0);
  await source.dragTo(target);

  // The backend was asked for the NEW order, second tile first.
  await expect
    .poll(() =>
      page.evaluate(() => (window as unknown as { __reorder?: string[] }).__reorder),
    )
    .toEqual(["t-b", "t-a"]);
});

test("Dashboards: the tile palette escapes every containing block and lands in the viewport", async ({
  page,
}) => {
  // REGRESSION GUARD. A `position: fixed` overlay only anchors to the VIEWPORT when no
  // ancestor establishes a fixed-positioning containing block — any ancestor with
  // transform / filter / backdrop-filter / contain becomes one, and the board canvas and
  // the Ask column are both frosted surfaces. The palette shipped WITHOUT the teleport
  // `mur-source-picker` uses for exactly this reason, which is how it could open fine in
  // one engine and be unreachable in the packaged WKWebView.
  await mockTauri(
    page,
    {},
    {
      list_dashboards: BOARDS,
      get_dashboard: { ...BOARDS[0], tiles: [] },
      get_dashboard_sources: [],
      list_link_candidates: [],
      get_graph: { nodes: [], edges: [], hasHidden: false },
    },
  );

  await page.goto("/dashboards/b-atlas");
  await page.getByRole("button", { name: "Add tile" }).click();

  // The trigger reflects state, so "the click landed" and "the palette rendered"
  // are separately observable — the two failures that looked identical in the
  // original bug report.
  await expect(page.getByRole("button", { name: "Close" })).toBeVisible();

  const palette = page.getByRole("dialog", { name: "Add a tile" });
  await expect(palette).toBeVisible();

  // It must be in the browser's TOP LAYER — that is what puts it outside every
  // stacking context and every fixed-positioning containing block. `:modal`
  // matches only a dialog opened with showModal(), not one merely `open`.
  const isTopLayer = await page.evaluate(() => {
    const el = document.querySelector("dialog.palette");
    return !!el && el.matches(":modal");
  });
  expect(isTopLayer, "the palette must be a showModal() dialog in the top layer").toBe(true);

  // And it must be fully on screen — the failure mode is "opens, but off-viewport".
  const box = await palette.boundingBox();
  const viewport = page.viewportSize();
  expect(box).not.toBeNull();
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.y).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(viewport!.width + 1);
  expect(box!.y + box!.height).toBeLessThanOrEqual(viewport!.height + 1);

  // The catalogue is actually populated — an empty palette is the same dead end.
  expect(await palette.locator(".node").count()).toBe(10);

  // HIT TEST: the palette's centre must actually BE the palette. "Rendered, on
  // screen, but covered by something" looks identical to the user to "did not
  // open", and neither a visibility check nor a bounding box catches it.
  const hit = await page.evaluate(() => {
    const el = document.querySelector(".palette");
    if (!el) return "no-palette";
    const r = el.getBoundingClientRect();
    const top = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
    return el.contains(top) ? "palette" : (top?.className?.toString() ?? "unknown");
  });
  expect(hit, "the palette centre must be hit-testable, not covered").toBe("palette");

  // And a catalogue entry must be clickable end to end (it advances to step 2).
  await palette.locator(".node").first().click();
  await expect(palette.getByRole("button", { name: /All tiles/ })).toBeVisible();
});
