import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

const BOARD = {
  id: "b-living-answer",
  title: "Atlas decisions",
  emoji: "🧭",
  tint: "indigo",
  pinned: true,
  position: 0,
  createdAt: "2026-08-01T09:00:00Z",
  updatedAt: "2026-08-13T09:00:00Z",
  tileCount: 1,
  tileKinds: [],
};

const QUESTION = "Are we ready to launch Atlas?";

function livingAnswer(
  answer: string | null,
  answeredAt: string | null,
  withheld = false,
) {
  return {
    ...BOARD,
    tiles: [
      {
        id: "t-living-answer",
        dashboardId: BOARD.id,
        kind: "living_answer",
        refId: null,
        title: null,
        span: 5,
        position: 0,
        config: null,
        createdAt: "2026-08-01T09:00:00Z",
        data: {
          kind: "livingAnswer",
          question: QUESTION,
          answer,
          answeredAt,
          withheld,
        },
      },
    ],
  };
}

test("Living Answer refresh sends identity and question only, then reloads the readable board", async ({
  page,
}) => {
  const refreshedAnswer =
    "Yes — the launch checklist is complete and the current board has no blockers.";
  const initial = livingAnswer(null, null);
  const refreshed = livingAnswer(refreshedAnswer, "2026-08-13T10:00:00Z");

  await mockTauri(
    page,
    {
      get_dashboard: () => {
        const root = globalThis as {
          __livingBoard: unknown;
          __livingBoardReads?: number;
        };
        root.__livingBoardReads = (root.__livingBoardReads ?? 0) + 1;
        return root.__livingBoard;
      },
      refresh_dashboard_answer: (args: unknown) => {
        const root = globalThis as {
          __livingRefreshArgs?: unknown;
          __refreshedLivingBoard: {
            tiles: Array<{ data: unknown }>;
          };
          __livingBoard: unknown;
        };
        root.__livingRefreshArgs = args;
        root.__livingBoard = root.__refreshedLivingBoard;
        return root.__refreshedLivingBoard.tiles[0].data;
      },
      set_dashboard_answer: () => {
        const root = globalThis as { __legacyAnswerWrites?: number };
        root.__legacyAnswerWrites = (root.__legacyAnswerWrites ?? 0) + 1;
        return null;
      },
    },
    {
      list_dashboards: [BOARD],
    },
  );
  await page.addInitScript(
    ({ initialBoard, refreshedBoard }) => {
      (globalThis as { __livingBoard: unknown }).__livingBoard = initialBoard;
      (
        globalThis as { __refreshedLivingBoard: unknown }
      ).__refreshedLivingBoard = refreshedBoard;
    },
    { initialBoard: initial, refreshedBoard: refreshed },
  );

  await page.goto(`/dashboards/${BOARD.id}`);
  await page.getByRole("button", { name: "Answer now" }).click();

  await expect(page.getByText(refreshedAnswer, { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Re-answer" })).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (globalThis as { __livingBoardReads?: number }).__livingBoardReads ??
          0,
      ),
    )
    .toBe(2);

  const payload = await page.evaluate(
    () =>
      (globalThis as { __livingRefreshArgs?: Record<string, unknown> })
        .__livingRefreshArgs,
  );
  expect(Object.keys(payload ?? {}).sort()).toEqual([
    "dashboardId",
    "question",
    "tileId",
  ]);
  expect(payload).toEqual({
    dashboardId: BOARD.id,
    tileId: "t-living-answer",
    question: QUESTION,
  });
  expect(payload).not.toHaveProperty("answer");
  expect(payload).not.toHaveProperty("answerSources");
  expect(payload).not.toHaveProperty("provenance");
  expect(
    await page.evaluate(
      () =>
        (globalThis as { __legacyAnswerWrites?: number })
          .__legacyAnswerWrites ?? 0,
    ),
  ).toBe(0);
});

test("Living Answer refresh cannot land after a board mutation", async ({
  page,
}) => {
  const lateAnswer = "LATE MUTATION ANSWER MUST NOT LAND";
  await mockTauri(
    page,
    {
      refresh_dashboard_answer: () =>
        new Promise((resolve) => {
          (
            globalThis as { __releaseMutationAnswer?: () => void }
          ).__releaseMutationAnswer = () =>
            resolve({
              kind: "livingAnswer",
              question: "Are we ready to launch Atlas?",
              answer: "LATE MUTATION ANSWER MUST NOT LAND",
              answeredAt: "2026-08-13T10:00:00Z",
              withheld: false,
            });
        }),
      delete_dashboard_tile: () => true,
    },
    {
      list_dashboards: [BOARD],
      get_dashboard: livingAnswer(
        "The earlier answer.",
        "2026-08-12T10:00:00Z",
      ),
    },
  );

  await page.goto(`/dashboards/${BOARD.id}`);
  await page.getByRole("button", { name: "Re-answer" }).click();
  await expect(page.getByRole("button", { name: "Refreshing…" })).toBeVisible();
  await page.getByRole("button", { name: "Compose", exact: true }).click();
  await page
    .getByRole("button", { name: `Remove ${QUESTION} from board` })
    .click();
  await page.evaluate(() =>
    (
      globalThis as { __releaseMutationAnswer?: () => void }
    ).__releaseMutationAnswer?.(),
  );

  await expect(page.locator("body")).not.toContainText(lateAnswer);
  await expect(page.locator('[data-tile-id="t-living-answer"]')).toHaveCount(0);
});

test("Living Answer refresh never renders a backend-withheld payload", async ({
  page,
}) => {
  const withheldSecret = "WITHHELD RESPONSE SENTINEL MUST NOT LAND";
  await mockTauri(
    page,
    {
      refresh_dashboard_answer: () => ({
        kind: "livingAnswer",
        question: "Are we ready to launch Atlas?",
        answer: "WITHHELD RESPONSE SENTINEL MUST NOT LAND",
        answeredAt: "2026-08-13T10:00:00Z",
        withheld: true,
      }),
    },
    {
      list_dashboards: [BOARD],
      get_dashboard: livingAnswer(
        "The earlier answer remains readable.",
        "2026-08-12T10:00:00Z",
      ),
    },
  );

  await page.goto(`/dashboards/${BOARD.id}`);
  await page.getByRole("button", { name: "Re-answer" }).click();

  await expect(
    page.getByText("The earlier answer remains readable."),
  ).toBeVisible();
  await expect(page.locator("body")).not.toContainText(withheldSecret);
  expect(
    await page.evaluate(
      (secret) => document.documentElement.outerHTML.includes(secret),
      withheldSecret,
    ),
  ).toBe(false);
});

test("Living Answer refresh cannot cross a privacy invalidation or render a withheld response", async ({
  page,
}) => {
  const withheldSecret = "WITHHELD NIGHTJAR ANSWER MUST NOT LAND";
  const readable = livingAnswer(
    "The earlier readable answer.",
    "2026-08-12T10:00:00Z",
  );
  const locked = {
    ...BOARD,
    tiles: [
      {
        ...readable.tiles[0],
        title: withheldSecret,
        data: { kind: "locked" },
      },
    ],
  };

  await mockTauri(
    page,
    {
      get_dashboard: () => {
        const root = globalThis as {
          __livingLocked?: boolean;
          __readableLivingBoard: unknown;
          __lockedLivingBoard: unknown;
        };
        return root.__livingLocked
          ? root.__lockedLivingBoard
          : root.__readableLivingBoard;
      },
      refresh_dashboard_answer: () =>
        new Promise((resolve) => {
          (
            globalThis as { __releaseWithheldAnswer?: () => void }
          ).__releaseWithheldAnswer = () =>
            resolve({
              kind: "livingAnswer",
              question: "Are we ready to launch Atlas?",
              answer: "WITHHELD NIGHTJAR ANSWER MUST NOT LAND",
              answeredAt: "2026-08-13T10:00:00Z",
              withheld: true,
            });
        }),
    },
    {
      list_dashboards: [BOARD],
    },
  );
  await page.addInitScript(
    ({ readableBoard, lockedBoard }) => {
      (globalThis as { __livingLocked?: boolean }).__livingLocked = false;
      (globalThis as { __readableLivingBoard: unknown }).__readableLivingBoard =
        readableBoard;
      (globalThis as { __lockedLivingBoard: unknown }).__lockedLivingBoard =
        lockedBoard;
    },
    { readableBoard: readable, lockedBoard: locked },
  );

  await page.goto(`/dashboards/${BOARD.id}`);
  await page.getByRole("button", { name: "Re-answer" }).click();
  await expect(page.getByRole("button", { name: "Refreshing…" })).toBeVisible();
  await page.evaluate(() => {
    const root = globalThis as {
      __livingLocked?: boolean;
      __demoEmit: (event: string, payload: unknown) => void;
    };
    root.__livingLocked = true;
    root.__demoEmit("murmur://ask-history-invalidated", null);
  });
  await page.evaluate(() =>
    (
      globalThis as { __releaseWithheldAnswer?: () => void }
    ).__releaseWithheldAnswer?.(),
  );

  await expect(page.getByText("1 sealed and excluded")).toBeVisible();
  await expect(page.locator("body")).not.toContainText(withheldSecret);
  expect(
    await page.evaluate(
      (secret) => document.documentElement.outerHTML.includes(secret),
      withheldSecret,
    ),
  ).toBe(false);
});
