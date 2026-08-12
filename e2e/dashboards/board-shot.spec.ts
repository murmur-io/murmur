import { test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * NOT a gate — a LOOK. This reproduces the exact board from the 2026-08-04
 * complaint (nine tiles, five of them empty, two identical Promise ledgers) and
 * writes a screenshot, because the defect being fixed is one that every green
 * gate in the repo was blind to. Kept out of the CI grep by having no assertions.
 */

const BOARD = {
  id: "b-test",
  title: "Test",
  emoji: null,
  tint: null,
  pinned: false,
  position: 0,
  createdAt: "2026-07-20T09:00:00Z",
  updatedAt: "2026-08-03T09:00:00Z",
  tileCount: 9,
  tileKinds: [],
};

const PROMISE_ROWS = [
  {
    text: 'Weronika — zweryfikować nazwę "Alcon" (duplikat)',
    meta: "Weronika · due 2026-07-06",
    status: "late",
    source: null,
  },
  {
    text: "Leszek — sprawdzić, czy rampa do kupienia jest dostępna",
    meta: "Leszek · due 2026-07-07",
    status: "late",
    source: null,
  },
  {
    text: "Organizator - ustalenie dokładnej liczby osób",
    meta: "Organizator · due 2026-07-10",
    status: "late",
    source: null,
  },
  {
    text: "Organizator — sprawdzić, czy dzieci mają miejsca",
    meta: "Organizator · due 2026-07-10",
    status: "late",
    source: null,
  },
  {
    text: "Organizator — ustal czas rozpoczęcia spotkania",
    meta: "Organizator · due 2026-07-11",
    status: "late",
    source: null,
  },
  {
    text: "Mówca — skorzystać z toalety przed wyjściem",
    meta: "Mówca",
    status: "open",
    source: null,
  },
];

function t(id: string, kind: string, data: unknown, position: number) {
  return {
    id,
    dashboardId: "b-test",
    kind,
    refId: `r-${position}`,
    title: null,
    span: 4,
    position,
    config: null,
    createdAt: "2026-07-20T09:00:00Z",
    data,
  };
}

const TILES = [
  t(
    "t1",
    "note",
    {
      kind: "note",
      id: "n1",
      title: "Meeting 2026-07-20 02:30",
      snippet: "",
      updatedAt: Date.now() - 6 * 864e5,
    },
    0,
  ),
  t(
    "t2",
    "note",
    {
      kind: "note",
      id: "n2",
      title: "Jakub Gawroński CV Summary",
      snippet: "Jakub Gawroński CV",
      updatedAt: Date.now() - 16 * 864e5,
    },
    1,
  ),
  t(
    "t3",
    "meeting",
    {
      kind: "meeting",
      id: "m1",
      title: "Krytyka autopromocji prawnika (Ostrowski) — moment",
      startedAt: "2026-07-31T14:00:00Z",
      durationS: 240,
      hasAudio: true,
    },
    2,
  ),
  t("t4", "promises", { kind: "promises", owner: null, rows: PROMISE_ROWS }, 3),
  t("t5", "promises", { kind: "promises", owner: null, rows: PROMISE_ROWS }, 4),
  t(
    "t6",
    "pulse",
    {
      kind: "pulse",
      entity: "Kuba",
      weekly: [0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0],
      total: 2,
      quietDays: 30,
    },
    5,
  ),
  t("t7", "numbers", { kind: "numbers", entity: "Brain", rows: [] }, 6),
  t(
    "t8",
    "drift",
    {
      kind: "drift",
      entity: "Kuba",
      predicate: "role",
      rows: [
        { text: "owner", meta: "Jul 4, 2026", status: "now", source: null },
      ],
    },
    7,
  ),
  t(
    "t9",
    "note",
    {
      kind: "note",
      id: "n3",
      title: "Krytyka autopromocji prawnika (Ostrowski) — moment",
      snippet: "",
      updatedAt: Date.now() - 4 * 864e5,
    },
    8,
  ),
];

test("shot: the complained-about board, rebuilt", async ({ page }) => {
  await mockTauri(
    page,
    {},
    {
      list_dashboards: [BOARD],
      get_dashboard: { ...BOARD, tiles: TILES },
      get_dashboard_sources: [
        { kind: "note", id: "n2" },
        { kind: "meeting", id: "m1" },
      ],
    },
  );
  await page.setViewportSize({ width: 1680, height: 1050 });
  await page.goto("/dashboards/b-test");
  await page.locator('section[aria-label="Board brief"]').waitFor();
  await page.waitForTimeout(400);
  await page.screenshot({
    path: "test-results/board-after.png",
    fullPage: true,
  });
});
