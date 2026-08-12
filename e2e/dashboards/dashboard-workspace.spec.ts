import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

const SEALED_SECRET = "Project Nightjar confidential acquisition";

const BOARD = {
  id: "b-workspace",
  title: "Atlas workspace",
  emoji: "🧭",
  tint: "indigo",
  pinned: true,
  position: 0,
  createdAt: "2026-08-01T09:00:00Z",
  updatedAt: "2026-08-11T09:00:00Z",
  tileCount: 9,
  tileKinds: [],
};

function tile(
  id: string,
  kind: string,
  refId: string | null,
  data: unknown,
  position: number,
) {
  return {
    id,
    dashboardId: BOARD.id,
    kind,
    refId,
    title: null,
    span: 4,
    position,
    config: null,
    createdAt: "2026-08-01T09:00:00Z",
    data,
  };
}

const TILES = [
  tile(
    "t-note",
    "note",
    "n-plan",
    {
      kind: "note",
      id: "n-plan",
      title: "Atlas launch plan",
      snippet: "The rollout starts with the private beta and support handoff.",
      updatedAt: 1_786_400_000_000,
    },
    0,
  ),
  tile(
    "t-recording",
    "meeting",
    "m-review",
    {
      kind: "meeting",
      id: "m-review",
      title: "Atlas launch review",
      startedAt: "2026-08-10T09:00:00Z",
      durationS: 1860,
      hasAudio: true,
    },
    1,
  ),
  tile(
    "t-document",
    "document",
    "d-prd",
    {
      kind: "document",
      id: "d-prd",
      title: "Atlas PRD",
      snippet: "Success criteria, rollout risks and the launch checklist.",
    },
    2,
  ),
  tile(
    "t-promises",
    "promises",
    null,
    {
      kind: "promises",
      owner: null,
      rows: [
        {
          text: "Kuba — confirm the release checklist",
          meta: "due 2026-08-11",
          status: "late",
          source: { kind: "meeting", id: "m-review" },
        },
        {
          text: "Marta — send the support handoff",
          meta: "open",
          status: "open",
          source: null,
        },
      ],
    },
    3,
  ),
  tile(
    "t-reminders",
    "reminders",
    null,
    {
      kind: "reminders",
      dueCount: 1,
      rows: [
        {
          text: "Review the launch risk register",
          meta: "today",
          status: "due",
          source: { kind: "note", id: "n-plan" },
        },
      ],
    },
    4,
  ),
  tile(
    "t-good-zero",
    "promises",
    "owner-closed",
    { kind: "promises", owner: "Closed team", rows: [] },
    5,
  ),
  tile(
    "t-person",
    "person",
    "person-kuba",
    {
      kind: "person",
      id: "person-kuba",
      name: "Kuba",
      mentionCount: 12,
      openCommitments: 2,
    },
    6,
  ),
  tile(
    "t-answer",
    "living_answer",
    null,
    {
      kind: "livingAnswer",
      question: "Are we ready to launch Atlas?",
      answer: "The launch is viable once the overdue checklist is confirmed.",
      answeredAt: "2026-08-11T09:00:00Z",
      withheld: false,
    },
    7,
  ),
  {
    ...tile("t-locked", "note", "n-locked", { kind: "locked" }, 8),
    title: SEALED_SECRET,
    config: JSON.stringify({ legacyTitle: SEALED_SECRET }),
  },
];

const DETAIL = { ...BOARD, tiles: TILES };
const SOURCES = [
  { kind: "note", id: "n-plan" },
  { kind: "meeting", id: "m-review" },
  { kind: "document", id: "d-prd" },
];

async function openBoard(page: import("@playwright/test").Page): Promise<void> {
  await mockTauri(
    page,
    {},
    {
      list_dashboards: [BOARD],
      get_dashboard: DETAIL,
      get_dashboard_sources: SOURCES,
    },
  );
  await page.goto(`/dashboards/${BOARD.id}`);
  await page.locator('section[aria-label="Board brief"]').waitFor();
}

async function enterCompose(
  page: import("@playwright/test").Page,
): Promise<void> {
  await page.getByRole("button", { name: "Compose", exact: true }).click();
  await page.locator('section[aria-label="Compose board tiles"]').waitFor();
}

async function assertNoSealedSecret(
  page: import("@playwright/test").Page,
): Promise<void> {
  expect(
    await page.evaluate(
      (secret) => document.documentElement.outerHTML.includes(secret),
      SEALED_SECRET,
    ),
    "a resolved locked tile must not restore legacy content-shaped chrome",
  ).toBe(false);
}

test("Dashboard workspace: Brief is the calm default and every lens represents the same board", async ({
  page,
}) => {
  await openBoard(page);

  const read = page.locator('section[aria-label="Board brief"]');
  await expect(read.locator(".dominant")).toHaveCount(1);
  await expect(read.getByText("Are we ready to launch Atlas?")).toBeVisible();
  await expect(
    read.getByRole("heading", { name: "Needs attention" }),
  ).toBeVisible();
  await expect(
    read.getByText("Kuba — confirm the release checklist"),
  ).toBeVisible();
  await expect(
    read.getByRole("heading", { name: "Recent evidence" }),
  ).toBeVisible();
  await expect(read.getByText("Atlas launch plan")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Brief", exact: true }),
  ).toHaveAttribute("aria-current", "page");
  await assertNoSealedSecret(page);

  await page.getByRole("button", { name: "Overview", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "The board at a glance" }),
  ).toBeVisible();
  await expect(page.getByText("Readable sources")).toBeVisible();
  await expect(page.getByText("3", { exact: true }).first()).toBeVisible();
  await assertNoSealedSecret(page);

  await page.getByRole("button", { name: "Commitments", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Promises and reminders" }),
  ).toBeVisible();
  await expect(page.getByText("Review the launch risk register")).toBeVisible();
  await assertNoSealedSecret(page);

  await page.getByRole("button", { name: "Sources", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Material in this board" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: /Atlas launch review/ }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: /Atlas PRD/ })).toBeVisible();
  await assertNoSealedSecret(page);

  await page.getByRole("button", { name: "People", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Explicit people views" }),
  ).toBeVisible();
  await expect(page.getByText("Kuba", { exact: true })).toBeVisible();
  await expect(page.getByText("12 meetings")).toBeVisible();
  await assertNoSealedSecret(page);
});

test("Dashboard workspace: superseded drift history is explicit and distinct from unavailable states", async ({
  page,
}) => {
  const history = {
    ...BOARD,
    tiles: [
      tile(
        "t-superseded-history",
        "drift",
        "entity-atlas",
        {
          kind: "drift",
          entity: "Project Atlas",
          predicate: "launch date",
          rows: [
            {
              text: "30 April",
              meta: "recorded 12 March",
              status: "old",
              source: { kind: "meeting", id: "m-history" },
            },
            {
              text: "14 June",
              meta: "recorded 3 June",
              status: "now",
              source: { kind: "meeting", id: "m-current" },
            },
          ],
        },
        0,
      ),
      tile("t-missing-history", "note", "n-missing", { kind: "missing" }, 1),
      tile("t-locked-history", "note", "n-locked", { kind: "locked" }, 2),
    ],
  };
  await mockTauri(
    page,
    {},
    {
      list_dashboards: [BOARD],
      get_dashboard: history,
      get_dashboard_sources: [],
    },
  );

  await page.goto(`/dashboards/${BOARD.id}`);
  const boundary = page.locator('footer[aria-label="Board boundary"]');
  await expect(boundary).toContainText("1 superseded past-state value");
  await expect(boundary).toContainText("1 missing or not configured");
  await expect(boundary).toContainText("1 sealed and excluded");

  await page.getByRole("button", { name: "Overview", exact: true }).click();
  const overview = page.getByRole("region", { name: "overview lens" });
  await expect(overview.getByText("Superseded values")).toBeVisible();
  await expect(overview.getByText("Superseded past state")).toBeVisible();
  await expect(
    overview.getByText("Project Atlas · launch date: 30 April"),
  ).toBeVisible();
  await expect(overview.getByText("past state", { exact: true })).toBeVisible();
  const superseded = overview.locator(
    'section[aria-labelledby="superseded-title"]',
  );
  await expect(superseded).not.toContainText("14 June");
  await expect(superseded).not.toContainText(/empty|stale|sealed|missing/i);
});

test("Dashboard workspace: Note and Recording additions persist ref ids only and restore focus", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      add_dashboard_tile: (args: unknown) => {
        const root = globalThis as { __dashboardAddCalls?: unknown[] };
        root.__dashboardAddCalls = [...(root.__dashboardAddCalls ?? []), args];
        return null;
      },
    },
    {
      list_dashboards: [BOARD],
      get_dashboard: DETAIL,
      get_dashboard_sources: SOURCES,
      list_link_candidates: [
        {
          kind: "note",
          id: "n-candidate",
          title: "Sensitive candidate note title",
          snippet: "Sensitive candidate note content",
        },
        {
          kind: "meeting",
          id: "m-candidate",
          title: "Private recording title",
          snippet: "Private recording transcript excerpt",
        },
        {
          kind: "document",
          id: "d-candidate",
          title: "Private strategy deck",
          snippet: "Private document extract",
        },
      ],
    },
  );
  await page.goto(`/dashboards/${BOARD.id}`);
  await enterCompose(page);

  const invoker = page.getByRole("button", { name: "Add to board" });
  await invoker.click();
  let palette = page.getByRole("dialog", { name: "Add to board" });
  await expect(palette).toBeVisible();
  await expect(palette.getByRole("heading", { name: "Sources" })).toBeVisible();
  await expect(palette.getByRole("heading", { name: "Views" })).toBeVisible();
  await expect(palette.getByRole("button", { name: /^Note/ })).toBeFocused();

  // Shift+Tab from the first choice wraps to the last, and Tab wraps back. Focus
  // cannot escape to the board beneath the opaque overlay.
  await page.keyboard.press("Shift+Tab");
  await expect(palette.locator(".node").last()).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(palette.getByRole("button", { name: /^Note/ })).toBeFocused();

  await palette.getByRole("button", { name: /^Note/ }).click();
  await expect(
    palette.getByRole("textbox", { name: "Search sources" }),
  ).toBeFocused();
  await expect(
    palette.getByText("Sensitive candidate note title"),
  ).toBeVisible();
  await expect(palette.getByText("Private recording title")).toHaveCount(0);

  // Escape backs out one level, keeping the overlay open; a second selection can
  // then complete the flow without losing the original trigger.
  await page.keyboard.press("Escape");
  await expect(palette.getByRole("heading", { name: "Sources" })).toBeVisible();
  await expect(palette.getByRole("button", { name: /^Note/ })).toBeFocused();
  await palette.getByRole("button", { name: /^Note/ }).click();
  await palette
    .getByRole("button", { name: /Sensitive candidate note title/ })
    .click();
  await expect(palette).toHaveCount(0);
  await expect(invoker).toBeFocused();

  await invoker.click();
  palette = page.getByRole("dialog", { name: "Add to board" });
  await palette.getByRole("button", { name: /^Recording/ }).click();
  await expect(
    palette.getByRole("textbox", { name: "Search sources" }),
  ).toBeFocused();
  await expect(palette.getByText("Private recording title")).toBeVisible();
  await expect(palette.getByText("Sensitive candidate note title")).toHaveCount(
    0,
  );
  await palette
    .getByRole("button", { name: /Private recording title/ })
    .click();
  await expect(palette).toHaveCount(0);
  await expect(invoker).toBeFocused();

  const calls = (await page.evaluate(
    () =>
      (globalThis as { __dashboardAddCalls?: unknown[] }).__dashboardAddCalls ??
      [],
  )) as Array<Record<string, unknown>>;
  expect(
    calls.map(({ dashboardId, kind, refId }) => ({ dashboardId, kind, refId })),
  ).toEqual([
    { dashboardId: BOARD.id, kind: "note", refId: "n-candidate" },
    { dashboardId: BOARD.id, kind: "meeting", refId: "m-candidate" },
  ]);
  for (const call of calls) {
    expect(Object.keys(JSON.parse(JSON.stringify(call))).sort()).toEqual([
      "dashboardId",
      "kind",
      "refId",
    ]);
    expect(call["title"]).toBeUndefined();
    expect(call["config"]).toBeUndefined();
  }
  const wire = JSON.stringify(calls);
  expect(wire).not.toContain("Sensitive candidate note title");
  expect(wire).not.toContain("Sensitive candidate note content");
  expect(wire).not.toContain("Private recording title");
  expect(wire).not.toContain("Private recording transcript excerpt");
});

test("Dashboard workspace: keyboard tile movement persists the same order shown in Compose", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      reorder_dashboard_tiles: async (args: unknown) => {
        (globalThis as { __dashboardReorder?: unknown }).__dashboardReorder =
          args;
        await new Promise((resolve) => setTimeout(resolve, 250));
        return null;
      },
    },
    {
      list_dashboards: [BOARD],
      get_dashboard: DETAIL,
      get_dashboard_sources: SOURCES,
    },
  );
  await page.goto(`/dashboards/${BOARD.id}`);
  await enterCompose(page);

  const first = page.locator("app-dashboard-tile").first();
  await expect(first).toHaveAttribute("data-tile-id", "t-note");
  await first.getByRole("button", { name: "Move tile later" }).click();
  await expect(page.locator("app-dashboard-tile").first()).toHaveAttribute(
    "data-tile-id",
    "t-recording",
  );
  await expect(page.locator("app-dashboard-tile").nth(1)).toHaveAttribute(
    "data-tile-id",
    "t-note",
  );

  const sent = (await expect
    .poll(() =>
      page.evaluate(
        () =>
          (globalThis as { __dashboardReorder?: unknown }).__dashboardReorder,
      ),
    )
    .toBeTruthy()
    .then(() =>
      page.evaluate(
        () =>
          (globalThis as { __dashboardReorder?: unknown }).__dashboardReorder,
      ),
    )) as { dashboardId?: string; tileIds?: string[] };
  expect(sent.dashboardId).toBe(BOARD.id);
  expect(sent.tileIds?.slice(0, 2)).toEqual(["t-recording", "t-note"]);
});

test("Dashboard workspace: 900x680 keeps primary actions and overlays usable without horizontal overflow", async ({
  page,
}) => {
  await page.setViewportSize({ width: 900, height: 680 });
  await openBoard(page);

  await expect(
    page.getByRole("button", { name: "Ask", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Compose", exact: true }),
  ).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    )
    .toBe(true);

  const cards = await page
    .locator(".brief-grid .read-card")
    .evaluateAll((elements) =>
      elements.map((element) => {
        const rect = element.getBoundingClientRect();
        return {
          left: rect.left,
          right: rect.right,
          top: rect.top,
          bottom: rect.bottom,
        };
      }),
    );
  expect(cards).toHaveLength(2);
  const overlaps =
    cards[0].left < cards[1].right &&
    cards[0].right > cards[1].left &&
    cards[0].top < cards[1].bottom &&
    cards[0].bottom > cards[1].top;
  expect(
    overlaps,
    "Brief cards must not overlap at the default Murmur window",
  ).toBe(false);

  await page.getByRole("button", { name: "Ask", exact: true }).click();
  await expect(page.locator("aside.ask")).toBeVisible();
  await expect(page.getByRole("button", { name: "Close Ask" })).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    )
    .toBe(true);
  await page.getByRole("button", { name: "Close Ask" }).click();

  await enterCompose(page);
  await page.getByRole("button", { name: "Add to board" }).click();
  const palette = page.getByRole("dialog", { name: "Add to board" });
  const box = await palette.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.x).toBeGreaterThanOrEqual(8);
  expect(box!.y).toBeGreaterThanOrEqual(8);
  expect(box!.x + box!.width).toBeLessThanOrEqual(892);
  expect(box!.y + box!.height).toBeLessThanOrEqual(672);
  await palette.getByRole("button", { name: /^Note/ }).scrollIntoViewIfNeeded();
  await expect(palette.getByRole("button", { name: /^Note/ })).toBeVisible();
  await palette
    .getByRole("button", { name: /^Recording/ })
    .scrollIntoViewIfNeeded();
  await expect(
    palette.getByRole("button", { name: /^Recording/ }),
  ).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    )
    .toBe(true);
});

test("Dashboard workspace: a null board is unavailable, never misrepresented as empty", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      list_dashboards: [BOARD],
      get_dashboard: null,
      get_dashboard_sources: [],
    },
  );
  await page.goto(`/dashboards/${BOARD.id}`);

  await expect(page.getByText("Board unavailable.")).toBeVisible();
  await expect(page.getByRole("button", { name: "Try again" })).toBeVisible();
  await expect(
    page.getByText("Start with material for this board."),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Add a Note, Recording or Document" }),
  ).toHaveCount(0);
});

test("Dashboard workspace: relock wins over an older same-board readable response", async ({
  page,
}) => {
  const READABLE_SECRET = "Redwood signing terms and private valuation";
  const readable = {
    ...BOARD,
    tiles: [
      {
        ...tile(
          "t-race",
          "note",
          "n-race",
          {
            kind: "note",
            id: "n-race",
            title: READABLE_SECRET,
            snippet: "Never re-admit this after relock.",
            updatedAt: 1_786_400_000_000,
          },
          0,
        ),
        title: READABLE_SECRET,
      },
    ],
  };
  const locked = {
    ...BOARD,
    tiles: [
      {
        ...tile("t-race", "note", "n-race", { kind: "locked" }, 0),
        title: READABLE_SECRET,
      },
    ],
  };

  await mockTauri(
    page,
    {
      get_dashboard: async () => {
        const root = globalThis as {
          __boardReads?: number;
          __releaseOldBoard?: () => void;
          __readableBoard: unknown;
          __lockedBoard: unknown;
        };
        root.__boardReads = (root.__boardReads ?? 0) + 1;
        if (root.__boardReads === 1) {
          await new Promise<void>((resolve) => {
            root.__releaseOldBoard = resolve;
          });
          return root.__readableBoard;
        }
        return root.__lockedBoard;
      },
      list_folders: () => {
        const root = globalThis as { __foldersRead?: number };
        root.__foldersRead = (root.__foldersRead ?? 0) + 1;
        const exposed = root.__foldersRead === 1;
        return [
          {
            id: "f-race",
            name: "Race folder",
            parentId: null,
            noteCount: 1,
            locked: true,
            unlocked: exposed,
            kind: "meeting",
            children: [],
          },
        ];
      },
      relock_all: () => null,
    },
    {
      list_dashboards: [BOARD],
      get_dashboard_sources: [],
    },
  );
  await page.addInitScript(
    ({ readableBoard, lockedBoard }) => {
      (globalThis as { __readableBoard: unknown }).__readableBoard =
        readableBoard;
      (globalThis as { __lockedBoard: unknown }).__lockedBoard = lockedBoard;
    },
    { readableBoard: readable, lockedBoard: locked },
  );
  await page.goto(`/dashboards/${BOARD.id}`);
  await expect
    .poll(() =>
      page.evaluate(
        () => (globalThis as { __boardReads?: number }).__boardReads ?? 0,
      ),
    )
    .toBe(1);

  // This is the real app-shell action. FoldersService.relockAll publishes the
  // fresh locked tree, and DashboardView must synchronously discard the old
  // content before issuing its newer same-id gated read.
  await page
    .getByRole("button", { name: "Re-seal all 1 unlocked folder now" })
    .click();
  await expect
    .poll(() =>
      page.evaluate(
        () => (globalThis as { __boardReads?: number }).__boardReads ?? 0,
      ),
    )
    .toBe(2);
  await page.evaluate(() =>
    (globalThis as { __releaseOldBoard?: () => void }).__releaseOldBoard?.(),
  );

  await expect(page.getByText("1 sealed and excluded")).toBeVisible();
  await expect(page.locator("body")).not.toContainText(READABLE_SECRET);
  expect(
    await page.evaluate(
      (secret) => document.documentElement.outerHTML.includes(secret),
      READABLE_SECRET,
    ),
  ).toBe(false);
});

test("Dashboard workspace: relock scrubs mounted plaintext even when the folder refresh fails", async ({
  page,
}) => {
  const BOARD_SECRET = "Nightjar board valuation and signing date";
  const ASK_SECRET = "Nightjar assistant-only risk summary";
  const PALETTE_SECRET = "Nightjar private candidate title";
  const readable = {
    ...BOARD,
    tiles: [
      {
        ...tile(
          "t-relock-failure",
          "note",
          "n-relock-failure",
          {
            kind: "note",
            id: "n-relock-failure",
            title: BOARD_SECRET,
            snippet: "Readable before the folder is re-sealed.",
            updatedAt: 1_786_400_000_000,
          },
          0,
        ),
        title: BOARD_SECRET,
      },
    ],
  };
  const locked = {
    ...BOARD,
    tiles: [
      {
        ...tile(
          "t-relock-failure",
          "note",
          "n-relock-failure",
          { kind: "locked" },
          0,
        ),
        title: BOARD_SECRET,
      },
    ],
  };

  await mockTauri(
    page,
    {
      get_dashboard: () => {
        const root = globalThis as {
          __dashboardLocked?: boolean;
          __dashboardReads?: number;
          __readableBoard: unknown;
          __lockedBoard: unknown;
        };
        root.__dashboardReads = (root.__dashboardReads ?? 0) + 1;
        return root.__dashboardLocked
          ? root.__lockedBoard
          : root.__readableBoard;
      },
      get_dashboard_sources: () =>
        (globalThis as { __dashboardLocked?: boolean }).__dashboardLocked
          ? []
          : [{ kind: "note", id: "n-relock-failure" }],
      ask_vault: () => ({
        answer: "Nightjar assistant-only risk summary",
        sources: [],
        citations: [],
      }),
      list_link_candidates: () => [
        {
          kind: "note",
          id: "n-private-candidate",
          title: "Nightjar private candidate title",
          snippet: "Candidate plaintext must leave the DOM on relock.",
        },
      ],
      list_folders: () => {
        const root = globalThis as { __foldersRead?: number };
        root.__foldersRead = (root.__foldersRead ?? 0) + 1;
        if (root.__foldersRead > 1) {
          throw new Error("simulated post-relock folder refresh failure");
        }
        return [
          {
            id: "f-relock-failure",
            name: "Relock failure folder",
            parentId: null,
            noteCount: 1,
            locked: true,
            unlocked: true,
            kind: "meeting",
            children: [],
          },
        ];
      },
      relock_all: () => {
        const root = globalThis as {
          __dashboardLocked?: boolean;
          __relockCalls?: number;
          __demoEmit?: (event: string, payload: unknown) => void;
        };
        root.__relockCalls = (root.__relockCalls ?? 0) + 1;
        root.__dashboardLocked = true;
        root.__demoEmit?.("murmur://ask-history-invalidated", null);
        return null;
      },
    },
    {
      list_dashboards: [BOARD],
    },
  );
  await page.addInitScript(
    ({ readableBoard, lockedBoard }) => {
      (globalThis as { __readableBoard: unknown }).__readableBoard =
        readableBoard;
      (globalThis as { __lockedBoard: unknown }).__lockedBoard = lockedBoard;
      (globalThis as { __dashboardLocked?: boolean }).__dashboardLocked = false;
    },
    { readableBoard: readable, lockedBoard: locked },
  );
  await page.goto(`/dashboards/${BOARD.id}`);
  await expect(page.getByRole("heading", { name: BOARD_SECRET })).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          globalThis as {
            __demoEventListenerRegistrationCount: (event: string) => number;
          }
        ).__demoEventListenerRegistrationCount(
          "murmur://ask-history-invalidated",
        ),
      ),
    )
    .toBe(1);

  // Mount an Ask answer and a source candidate at the same time. Both are
  // ephemeral plaintext surfaces that must be destroyed on the earliest privacy
  // boundary, regardless of whether the folder tree can publish a new stamp.
  await page.getByRole("button", { name: "Ask", exact: true }).click();
  await page
    .getByRole("textbox", { name: "Ask a question about this board" })
    .fill("What is the hidden risk?");
  await page
    .getByLabel("Ask this board")
    .getByRole("button", { name: "Ask", exact: true })
    .click();
  await expect(page.getByText(ASK_SECRET)).toBeVisible();
  await page.getByRole("button", { name: "Compose", exact: true }).click();
  const addInvoker = page.getByRole("button", { name: "Add to board" });
  await addInvoker.focus();
  await page.keyboard.press("Enter");
  const palette = page.getByRole("dialog", { name: "Add to board" });
  await palette.getByRole("button", { name: /^Note/ }).click();
  await expect(palette.getByText(PALETTE_SECRET)).toBeVisible();

  // The opaque scrim correctly makes shell chrome pointer-inert. A backend
  // privacy boundary can still arrive while the palette is mounted (for example
  // from another window or screen-share auto-lock), so invoke the real shell
  // handler's native button activation without weakening production hit-testing.
  await page
    .getByRole("button", { name: "Re-seal all 1 unlocked folder now" })
    .evaluate((button) => (button as HTMLButtonElement).click());

  await expect
    .poll(() =>
      page.evaluate(
        () => (globalThis as { __relockCalls?: number }).__relockCalls ?? 0,
      ),
    )
    .toBe(1);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (globalThis as { __dashboardReads?: number }).__dashboardReads ?? 0,
      ),
    )
    .toBe(2);
  await expect
    .poll(() =>
      page.evaluate(
        () => (globalThis as { __foldersRead?: number }).__foldersRead ?? 0,
      ),
    )
    .toBe(2);

  // `list_folders` rejected, so FoldersService retained the old unlocked tree.
  // The process-wide privacy event must nevertheless close/scrub every surface
  // and trigger a fresh gated board read, which now resolves generically locked.
  await expect(palette).toHaveCount(0);
  await expect(page.locator("aside.ask")).toHaveCount(0);
  await expect(page.getByText("1 sealed and excluded")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Re-seal all 1 unlocked folder now" }),
  ).toBeVisible();

  // Compose had unmounted Ask before relock. Reopening it proves that the old
  // in-memory turn was destroyed rather than merely hidden with the surface.
  await page.getByRole("button", { name: "Ask", exact: true }).click();
  await expect(page.locator("aside.ask")).toBeVisible();
  await expect(page.locator("aside.ask")).not.toContainText(ASK_SECRET);

  for (const secret of [BOARD_SECRET, ASK_SECRET, PALETTE_SECRET]) {
    await expect(page.locator("body")).not.toContainText(secret);
    expect(
      await page.evaluate(
        (value) => document.documentElement.outerHTML.includes(value),
        secret,
      ),
      `${secret} must not survive a relock whose folder refresh failed`,
    ).toBe(false);
  }
});

test("Dashboard workspace: a privacy-listener failure blocks every board read and retry", async ({
  page,
}) => {
  const BLOCKED_SECRET = "Nightjar must never cross a missing privacy listener";
  const readable = {
    ...BOARD,
    tiles: [
      tile(
        "t-listener-failure",
        "note",
        "n-listener-failure",
        {
          kind: "note",
          id: "n-listener-failure",
          title: BLOCKED_SECRET,
          snippet: "This fixture would leak if get_dashboard were called.",
          updatedAt: 1_786_400_000_000,
        },
        0,
      ),
    ],
  };

  await mockTauri(
    page,
    {
      get_dashboard: () => {
        const root = globalThis as {
          __privacyBlockedReads?: number;
          __privacyBlockedBoard: unknown;
        };
        root.__privacyBlockedReads = (root.__privacyBlockedReads ?? 0) + 1;
        return root.__privacyBlockedBoard;
      },
    },
    {
      list_dashboards: [BOARD],
      get_dashboard_sources: [{ kind: "note", id: "n-listener-failure" }],
    },
    ["murmur://ask-history-invalidated"],
  );
  await page.addInitScript((board) => {
    (globalThis as { __privacyBlockedBoard: unknown }).__privacyBlockedBoard =
      board;
  }, readable);

  await page.goto(`/dashboards/${BOARD.id}`);
  await expect(page.getByText("Board unavailable.")).toBeVisible();
  await expect(
    page.getByText(/isn.t available securely right now/),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Try again" })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Add a Note, Recording or Document" }),
  ).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (globalThis as { __privacyBlockedReads?: number })
            .__privacyBlockedReads ?? 0,
      ),
    )
    .toBe(0);

  await page.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByText("Board unavailable.")).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (globalThis as { __privacyBlockedReads?: number })
            .__privacyBlockedReads ?? 0,
      ),
    )
    .toBe(0);
  await expect(page.locator("body")).not.toContainText(BLOCKED_SECRET);
  expect(
    await page.evaluate(
      (secret) => document.documentElement.outerHTML.includes(secret),
      BLOCKED_SECRET,
    ),
  ).toBe(false);
});
