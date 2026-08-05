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
  // the Ask column are both frosted surfaces. The palette used to render INSIDE that
  // subtree and lift itself back out (teleport, then top layer); it is now rendered by
  // `app-shell`, so there is nothing to lift it out of. This asserts both halves: where
  // it hangs in the DOM, and that it lands on screen and hit-testable.
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
  await expect(page.getByRole("button", { name: "Close", exact: true })).toBeVisible();

  const palette = page.getByRole("dialog", { name: "Add a tile" });
  await expect(palette).toBeVisible();

  // It must be rendered by `app-shell`, NOT inside the board. That is the whole
  // fix: an overlay whose only ancestors are <body>/<html> has no containing
  // block, stacking context or compositing decision to escape, so it needs no
  // teleport, no top layer and no `<dialog>` — none of which behaved the same in
  // the packaged webview as in the engines this suite runs.
  const ancestry = await page.evaluate(() => {
    const el = document.querySelector(".tp-overlay");
    if (!el) return { found: false, insideBoard: true, containingBlocks: ["missing"] };
    const containingBlocks: string[] = [];
    for (let n = el.parentElement; n; n = n.parentElement) {
      const cs = getComputedStyle(n);
      const traps = [
        cs.transform,
        cs.filter,
        cs.backdropFilter || cs.webkitBackdropFilter,
        cs.perspective,
        cs.contain,
        cs.willChange,
      ];
      if (traps.some((v) => v && v !== "none" && v !== "auto" && v !== "normal")) {
        containingBlocks.push(n.tagName.toLowerCase());
      }
    }
    return {
      found: true,
      insideBoard: !!el.closest("app-dashboard-view"),
      containingBlocks,
    };
  });
  expect(ancestry.found, "the palette overlay must render").toBe(true);
  expect(
    ancestry.insideBoard,
    "the palette must NOT render inside the board's own subtree",
  ).toBe(false);
  expect(
    ancestry.containingBlocks,
    "no ancestor may establish a fixed-positioning containing block",
  ).toEqual([]);

  // And it must be fully on screen — the failure mode is "opens, but off-viewport".
  const box = await palette.boundingBox();
  const viewport = page.viewportSize();
  expect(box).not.toBeNull();
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.y).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(viewport!.width + 1);
  expect(box!.y + box!.height).toBeLessThanOrEqual(viewport!.height + 1);

  // The catalogue is actually populated — an empty palette is the same dead end.
  // Asserted as "several, including the ones we know are offered" rather than an
  // exact count: the catalogue legitimately shrinks (three kinds were retired on
  // 2026-08-04 because they structurally could not fire), and a magic number turns
  // every such product decision into a false failure here.
  expect(await palette.locator(".node").count()).toBeGreaterThanOrEqual(6);
  await expect(palette.getByText("Promise ledger")).toBeVisible();

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

test("Dashboards: the palette still shows on an engine without :modal or showModal", async ({ page }) => {
  // THE REPORTED FAILURE, reproduced. The trigger flipped to "Close" — so the click
  // landed and the state was right — while nothing appeared on screen. That is what
  // a <dialog> looks like when showModal() throws: it never opens, so it stays
  // display:none. The palette no longer uses <dialog> at all, so this now guards
  // against REINTRODUCING one: an engine missing either API must change nothing.
  await page.addInitScript(() => {
    // Simulate an OLDER engine, which is what the report turned out to be:
    // `:modal` is unknown so matches() THROWS, and showModal() is refused. The
    // thrown SyntaxError used to escape afterNextRender, leaving the dialog
    // unopened — and an unopened <dialog> is display:none.
    const realMatches = Element.prototype.matches;
    Element.prototype.matches = function (sel: string) {
      if (typeof sel === "string" && sel.includes(":modal")) {
        throw new DOMException("':modal' is not a valid selector", "SyntaxError");
      }
      return realMatches.call(this, sel);
    };
    if (window.HTMLDialogElement) {
      HTMLDialogElement.prototype.showModal = function () {
        throw new Error("simulated: showModal refused");
      };
    }
  });
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

  const palette = page.getByRole("dialog", { name: "Add a tile" });
  await expect(palette, "a refused showModal must not leave the palette hidden").toBeVisible();
  expect(await palette.locator(".node").count()).toBeGreaterThanOrEqual(6);

  // On screen, and hit-testable — not merely present in the DOM.
  const box = await palette.boundingBox();
  const viewport = page.viewportSize();
  expect(box!.y).toBeGreaterThanOrEqual(0);
  expect(box!.y + box!.height).toBeLessThanOrEqual(viewport!.height + 1);
  const hit = await page.evaluate(() => {
    const el = document.querySelector(".palette")!;
    const r = el.getBoundingClientRect();
    return el.contains(document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2));
  });
  expect(hit, "the fallback palette must be clickable, not covered").toBe(true);
});

test("Dashboards: the palette's position does not depend on the board's own subtree", async ({
  page,
}) => {
  // THE ORACLE THE EARLIER GUARDS COULD NOT BE. Every previous fix put the palette
  // somewhere INSIDE the board's own component subtree and then relied on one
  // mechanism to lift it back out — a teleport, then a `position: fixed` box, then
  // the browser's TOP LAYER via showModal(). Each of those works in the engines
  // Playwright ships and each still failed in the packaged WKWebView, because each
  // is a thing an engine can implement differently.
  //
  // This test removes the engine from the question by making the environment hostile
  // in BOTH ways at once:
  //   * the board's subtree establishes a fixed-positioning containing block
  //     (`transform` — the same thing a frosted/animated ancestor does), and
  //   * showModal() is refused, so the top layer is not available to escape it.
  // A palette rendered inside the board is then pinned to a box 1200px down the
  // page and is off screen. A palette rendered by `app-shell` — whose only
  // ancestors are <body> and <html> — cannot be affected by either, which is the
  // property this asserts.
  await page.addInitScript(() => {
    if (window.HTMLDialogElement) {
      HTMLDialogElement.prototype.showModal = function () {
        throw new Error("simulated: no top layer on this engine");
      };
    }
  });
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
  await page.addStyleTag({
    content: "app-dashboard-view { transform: translateY(1200px); }",
  });
  await page.getByRole("button", { name: "Add tile" }).click();

  const palette = page.getByRole("dialog", { name: "Add a tile" });
  await expect(palette).toBeVisible();

  const box = await palette.boundingBox();
  const viewport = page.viewportSize();
  expect(box).not.toBeNull();
  expect(
    box!.y,
    "a transformed ancestor must not be able to push the palette off screen",
  ).toBeGreaterThanOrEqual(0);
  expect(box!.y + box!.height).toBeLessThanOrEqual(viewport!.height + 1);

  // And still hit-testable at its centre, so it is genuinely usable.
  const hit = await page.evaluate(() => {
    const el = document.querySelector(".palette");
    if (!el) return false;
    const r = el.getBoundingClientRect();
    return el.contains(document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2));
  });
  expect(hit, "the palette centre must be hit-testable").toBe(true);
});

test("Dashboards: a tile with a malformed payload cannot blank the rest of the UI", async ({
  page,
}) => {
  // THE ACTUAL BUG behind six fixes aimed at the palette. `TileData` is an internally-tagged
  // serde enum, and `rename_all` on an enum renames the VARIANTS, not the fields inside them —
  // so a meeting tile shipped `started_at` while `models.ts` declares `startedAt`. Reading
  // `undefined` made `formatDate` do `undefined.slice(0, 10)` and THROW, and an exception from a
  // template binding aborts the rest of that change-detection pass. Everything rendered after it
  // in the same pass went blank — including the Add-a-tile palette, which `app-shell` renders
  // later. A board of only note tiles was fine, so it looked like a tile-COUNT bug.
  //
  // The wire shape itself is pinned in Rust (`every_tile_payload_field_is_camel_case_on_the_wire`),
  // because a fixture written from the TypeScript type can only ever assert the shape it already
  // assumes. THIS test pins the blast radius instead: whatever arrives, one bad tile must cost one
  // wrong-looking cell and nothing more.
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));

  const brokenTiles = [
    {
      id: "t-note", dashboardId: "b-atlas", kind: "note", refId: "n-1", title: null,
      span: 4, position: 0, config: null, createdAt: "2026-08-01T09:00:00Z",
      data: { kind: "note", id: "n-1", title: "A NOTE", snippet: "s", updatedAt: null },
    },
    {
      // Exactly what the backend used to send: no `startedAt`, no `durationS`, no `hasAudio`.
      id: "t-rec", dashboardId: "b-atlas", kind: "meeting", refId: "m-1", title: null,
      span: 4, position: 1, config: null, createdAt: "2026-08-01T09:00:00Z",
      data: { kind: "meeting", id: "m-1", title: "A RECORDING", started_at: "2026-07-20T02:30:00Z", duration_s: 900, has_audio: true },
    },
  ];

  await mockTauri(
    page,
    {},
    {
      list_dashboards: BOARDS,
      get_dashboard: { ...BOARDS[0], tiles: brokenTiles },
      get_dashboard_sources: [],
      list_link_candidates: [],
      get_graph: { nodes: [], edges: [], hasHidden: false },
    },
  );

  await page.goto("/dashboards/b-atlas");
  await expect(page.getByText("A RECORDING")).toBeVisible();

  // The palette is rendered LATER in the same change-detection pass than the tiles, so it is the
  // thing a throwing tile binding takes down. It must still open, and be populated.
  await page.getByRole("button", { name: "Add tile" }).click();
  const palette = page.getByRole("dialog", { name: "Add a tile" });
  await expect(palette, "a malformed tile must not stop the palette rendering").toBeVisible();
  expect(
    await palette.locator(".node").count(),
    "the palette's catalogue must render, not just its box",
  ).toBeGreaterThanOrEqual(6);

  // And nothing threw: a binding that throws is what caused the blanking in the first place.
  expect(errors, "no uncaught error may escape a template binding").toEqual([]);
});
