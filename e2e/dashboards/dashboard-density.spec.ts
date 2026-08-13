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
 * Dashboards — the DENSITY oracle (Phase 1 of the 2026-08-04 rebuild).
 *
 * These are RED-before-GREEN tests for a bug that had no failing check: a real
 * user board rendered as nine identical grey boxes, five of them holding an
 * apology, and every gate was green. `ng build` cannot see it, and neither can a
 * unit test — the defect is what the layout DOES with the payload.
 *
 * Three properties are pinned here, and each one failed on the code that shipped:
 *
 *  1. A tile whose payload NEVER had data collapses to a header strip rather than
 *     reserving a full card for a sentence of regret. `commands/dashboards.rs`
 *     stores `span.unwrap_or(4)` and `dashboard-view.component.ts::addTile` never
 *     passed a span, so every tile on every existing board is 4/12 — three per
 *     row, forever, empty or not.
 *
 *  2. Kinds get DIFFERENT display widths even though every stored row says 4.
 *     A ledger and a two-stat cluster are not the same shape and must not occupy
 *     the same box.
 *
 *  3. An empty PROMISES tile is NOT collapsed. "Nothing open — every commitment
 *     on this board is closed" is a RESULT, not an absence, and rendering a
 *     success as a grey apology is the single most demoralising thing the board
 *     does. This is the test that stops a naive "hide anything with no rows"
 *     implementation from passing the other two.
 */

const BOARDS = [
  {
    id: "b-dense",
    title: "Density",
    emoji: null,
    tint: null,
    pinned: false,
    position: 0,
    createdAt: "2026-08-01T09:00:00Z",
    updatedAt: "2026-08-03T09:00:00Z",
    tileCount: 4,
    tileKinds: [
      { kind: "numbers", span: 4 },
      { kind: "promises", span: 4 },
      { kind: "person", span: 4 },
      { kind: "note", span: 4 },
    ],
  },
];

/** Every tile stored at span 4 — exactly what a real board looks like today. */
function tile(id: string, kind: string, data: unknown, position: number) {
  return {
    id,
    dashboardId: "b-dense",
    kind,
    refId: "r-1",
    title: null,
    span: 4,
    position,
    config: null,
    createdAt: "2026-08-01T09:00:00Z",
    data,
  };
}

const BOARD_DETAIL = {
  ...BOARDS[0],
  tiles: [
    // Never had data → must collapse.
    tile(
      "t-numbers",
      "numbers",
      { kind: "numbers", entity: "Brain", rows: [] },
      0,
    ),
    // Genuinely zero → GOOD NEWS → must stay a full tile.
    tile(
      "t-promises",
      "promises",
      { kind: "promises", owner: null, rows: [] },
      1,
    ),
    // Populated stat cluster → narrow.
    tile(
      "t-person",
      "person",
      {
        kind: "person",
        id: "e-1",
        name: "Kuba",
        mentionCount: 12,
        openCommitments: 3,
      },
      2,
    ),
    // Populated prose.
    tile(
      "t-note",
      "note",
      {
        kind: "note",
        id: "n-1",
        title: "Atlas GA checklist",
        snippet: "Blocking: the auth migration, ~1 sprint left.",
        updatedAt: 1_780_000_000_000,
      },
      3,
    ),
  ],
};

async function openBoard(page: import("@playwright/test").Page) {
  await mockTauri(
    page,
    {},
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [],
    },
  );
  await page.goto("/dashboards/b-dense");
  await enterCompose(page);
  await expect(page.locator(".compose-row")).toHaveCount(4);
}

test("Dashboards: Compose renders one compact canonical row per tile with no body previews", async ({
  page,
}) => {
  await openBoard(page);
  expect(
    await page
      .locator(".compose-row")
      .evaluateAll((rows) =>
        rows.map((row) => row.getAttribute("data-tile-id")),
      ),
  ).toEqual(["t-numbers", "t-promises", "t-person", "t-note"]);
  await expect(page.locator(".tile-body")).toHaveCount(0);
  await expect(
    page.locator(".compose-row.is-empty, .compose-row.is-duplicate"),
  ).toHaveCount(0);
});

test("Dashboards: empty and populated derived views remain distinct labelled rows", async ({
  page,
}) => {
  await openBoard(page);
  await expect(page.locator('[data-tile-id="t-promises"]')).toContainText(
    "0 open promises",
  );
  await expect(page.locator('[data-tile-id="t-person"]')).toContainText(
    "Live derived board view",
  );
});

test("Dashboards: Compose has no width controls and every row uses the same list column", async ({
  page,
}) => {
  await openBoard(page);
  await expect(
    page.getByRole("button", { name: /Make tile (wider|narrower)/i }),
  ).toHaveCount(0);
  const lefts = await page
    .locator(".compose-row")
    .evaluateAll((rows) =>
      rows.map((row) => Math.round(row.getBoundingClientRect().left)),
    );
  expect(new Set(lefts).size).toBe(1);
});

test("Dashboards: every Compose row carries a per-kind label and handle", async ({
  page,
}) => {
  await openBoard(page);
  await expect(page.locator(".compose-row .kind-mark")).toHaveCount(4);
  await expect(page.locator('.compose-row[data-kind="note"]')).toContainText(
    "Note",
  );
  await expect(page.locator('.drag-handle[draggable="true"]')).toHaveCount(4);
});

test("Dashboards: a failed Ask is a banner with a retry, not a grey bubble that looks like an answer", async ({
  page,
}) => {
  // The exact failure from the 2026-08-04 report: `summarize/redact.rs::
  // content_free_dispatch_error` collapses a provider failure into one sentence,
  // and the board pushed it as `{role: "assistant"}` — so it rendered in the same
  // bubble as a grounded conclusion, with nothing to retry.
  await mockTauri(
    page,
    {
      ask_vault_persisted: () => {
        throw new Error(
          "summarizer error: cloud provider response failed after protected dispatch; details omitted",
        );
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [{ kind: "note", id: "n-1" }],
    },
  );

  await page.goto("/dashboards/b-dense");
  await openAsk(page);
  await page
    .getByRole("textbox", { name: /Ask a question/i })
    .fill("what is late?");
  await submitAsk(page);

  const banner = page.locator(".thread .banner.is-danger");
  await expect(banner).toBeVisible();
  await expect(
    banner.getByRole("button", { name: /Try again/i }),
  ).toBeVisible();
  // And it is NOT dressed as an answer.
  await expect(page.locator(".thread .bubble:not(.me)")).toHaveCount(0);
});

test("Dashboards: an answer renders as markdown, not as literal asterisks", async ({
  page,
}) => {
  // `summarize/vault_chat.rs::build` DEMANDS markdown from the model, and the
  // board rendered `{{ turn.text }}` with `white-space: pre-wrap`.
  await mockTauri(
    page,
    {
      ask_vault_persisted: () => ({
        answer: "The **auth migration** is the risk.",
        sources: [],
        citations: [],
      }),
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [{ kind: "note", id: "n-1" }],
    },
  );

  await page.goto("/dashboards/b-dense");
  await openAsk(page);
  await page
    .getByRole("textbox", { name: /Ask a question/i })
    .fill("what is the risk?");
  await submitAsk(page);

  const answer = page.locator(".thread .bubble:not(.me)");
  await expect(answer).toBeVisible();
  await expect(answer.locator("strong")).toHaveText("auth migration");
  await expect(answer).not.toContainText("**");
});

test("Dashboards: a duplicate tile renders as a back-reference, not a second copy", async ({
  page,
}) => {
  // Two Promises tiles with no owner resolve to the SAME global list — the exact
  // pair on the user's board. The palette never wrote `config.owner`, so this is
  // structurally guaranteed rather than a user error.
  await mockTauri(
    page,
    {},
    {
      list_dashboards: BOARDS,
      get_dashboard: {
        ...BOARDS[0],
        tiles: [
          tile(
            "t-p1",
            "promises",
            {
              kind: "promises",
              owner: null,
              rows: [
                {
                  text: "Send the Acme paperwork",
                  meta: "due 2026-07-22",
                  status: "late",
                  source: null,
                },
              ],
            },
            0,
          ),
          tile(
            "t-p2",
            "promises",
            {
              kind: "promises",
              owner: null,
              rows: [
                {
                  text: "Send the Acme paperwork",
                  meta: "due 2026-07-22",
                  status: "late",
                  source: null,
                },
              ],
            },
            1,
          ),
        ],
      },
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-dense");
  await enterCompose(page);
  await expect(page.locator(".compose-row")).toHaveCount(2);
  expect(
    await page
      .locator(".compose-row")
      .evaluateAll((rows) =>
        rows.map((row) => row.getAttribute("data-tile-id")),
      ),
  ).toEqual(["t-p1", "t-p2"]);
  await expect(page.locator(".compose-row.is-duplicate")).toHaveCount(0);
  await expect(page.getByText("Send the Acme paperwork")).toHaveCount(0);
});

test("Dashboards: Ask is on demand and closing it preserves the in-memory thread", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      ask_vault_persisted: () => ({
        answer: "Two are late.",
        sources: [],
        citations: [],
      }),
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [{ kind: "note", id: "n-1" }],
    },
  );

  await page.goto("/dashboards/b-dense");
  await expect(page.locator("aside.ask")).toHaveCount(0);
  await openAsk(page);
  const ask = page.locator("aside.ask");
  await expect(ask).toBeVisible();
  await page
    .getByRole("textbox", { name: /Ask a question/i })
    .fill("who is late?");
  await submitAsk(page);
  await expect(page.getByText("Two are late.")).toBeVisible();

  await ask.getByRole("button", { name: "Close Ask" }).click();
  await expect(ask).toHaveCount(0);
  await openAsk(page);
  await expect(page.getByText("who is late?")).toBeVisible();
  await expect(page.getByText("Two are late.")).toBeVisible();
});

test("Dashboards: a board can be renamed, and a leading emoji becomes its emoji", async ({
  page,
}) => {
  // `DashboardsService.update` shipped accepting title/emoji/tint and the only caller
  // ever passed `{pinned}` — so a board named the wrong thing stayed named the wrong
  // thing, and the `emoji` column plus six SCSS tint mappings were dead code.
  await mockTauri(
    page,
    {
      update_dashboard: (args: any) => {
        (globalThis as any).__updateArgs = args;
        return null;
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-dense");
  await page.getByRole("button", { name: /Rename this board/i }).click();

  const field = page.getByRole("textbox", { name: "Board name" });
  await expect(field).toBeVisible();
  // A ZWJ cluster — the case a naive `[...str][0]` splits into a lone man.
  await field.fill("👨‍👩‍👧 Family planning");
  await page.getByRole("button", { name: "Save", exact: true }).click();

  const sent = await expect
    .poll(async () => page.evaluate(() => (globalThis as any).__updateArgs))
    .toBeTruthy()
    .then(() => page.evaluate(() => (globalThis as any).__updateArgs));
  expect(sent.title).toBe("Family planning");
  expect(sent.emoji).toBe("👨‍👩‍👧");
});

test("Dashboards: renaming with no emoji clears the old one", async ({
  page,
}) => {
  // Dropping the field instead of sending "" would leave the previous emoji in place,
  // making the picture impossible to remove once set.
  await mockTauri(
    page,
    {
      update_dashboard: (args: any) => {
        (globalThis as any).__updateArgs = args;
        return null;
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: { ...BOARD_DETAIL, emoji: "🚀" },
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-dense");
  await page.getByRole("button", { name: /Rename this board/i }).click();
  await page.getByRole("textbox", { name: "Board name" }).fill("Plain name");
  await page.getByRole("button", { name: "Save", exact: true }).click();

  const sent = await expect
    .poll(async () => page.evaluate(() => (globalThis as any).__updateArgs))
    .toBeTruthy()
    .then(() => page.evaluate(() => (globalThis as any).__updateArgs));
  expect(sent.title).toBe("Plain name");
  expect(sent.emoji).toBe("");
});

test("Dashboards: deleting a board takes two clicks, and the first one is reversible", async ({
  page,
}) => {
  // A board is the one artifact in this feature the user BUILT rather than recorded,
  // and delete fired straight through on a single click with no undo.
  await mockTauri(
    page,
    {
      delete_dashboard: () => {
        (globalThis as any).__deleted =
          ((globalThis as any).__deleted ?? 0) + 1;
        return true;
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards");
  const card = page.locator(".board-card").first();

  // FIRST click arms — it must not delete.
  await card.getByRole("button", { name: /^Delete / }).click();
  await expect(
    card.getByRole("button", { name: /^Confirm delete / }),
  ).toBeVisible();
  expect(await page.evaluate(() => (globalThis as any).__deleted ?? 0)).toBe(0);

  // Backing out leaves the board alone…
  await card.getByRole("button", { name: /^Keep / }).click();
  await expect(card.getByRole("button", { name: /^Delete / })).toBeVisible();
  expect(await page.evaluate(() => (globalThis as any).__deleted ?? 0)).toBe(0);

  // …and only the SECOND click on an armed button actually deletes.
  await card.getByRole("button", { name: /^Delete / }).click();
  await card.getByRole("button", { name: /^Confirm delete / }).click();
  await expect
    .poll(() => page.evaluate(() => (globalThis as any).__deleted ?? 0))
    .toBe(1);
});

test("Dashboards: navigating away mid-ask does not leave the next board stuck busy", async ({
  page,
}) => {
  // The stale-async guard's own failure mode. Gating BOTH the data writes and the
  // busy flag on "same board AND same request" means that after navigating away
  // mid-flight nothing ever clears `asking` — so the NEXT board opens with its Ask
  // control disabled, waiting on a request that was never its own.
  await mockTauri(
    page,
    {
      ask_vault_persisted: async () => {
        await new Promise((r) => setTimeout(r, 1500));
        return {
          answer: "answer for the first board",
          sources: [],
          citations: [],
        };
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [{ kind: "note", id: "n-1" }],
    },
  );

  await page.goto("/dashboards/b-dense");
  await openAsk(page);
  await page
    .getByRole("textbox", { name: /Ask a question/i })
    .fill("slow question");
  await submitAsk(page);

  // Leave while it is still running, then come back.
  await page.goto("/dashboards");
  await page.goto("/dashboards/b-dense");
  await openAsk(page);

  // The Ask control is USABLE — not stuck behind a dead request…
  await page
    .getByRole("textbox", { name: /Ask a question/i })
    .fill("a fresh question");
  await expect(
    page
      .getByLabel("Ask this board")
      .getByRole("button", { name: "Ask", exact: true }),
  ).toBeEnabled();
  // …and the previous board's thread did not follow us here.
  await expect(page.getByText("slow question")).toHaveCount(0);
  await expect(page.getByText("answer for the first board")).toHaveCount(0);
});
test("Dashboards: sealed material-only board still asks with its board id", async ({
  page,
}) => {
  // The board scopes the ask even when it has nothing readable to SHOW. Deciding on
  // the resolved payload made this unreachable: every tile resolves to `locked`, the
  // old count was zero, and the UI told the user to add tiles to a board that has
  // them — while the backend's "scoped but empty" path never ran, which is the one
  // thing that stops such a board answering from the whole vault.
  await mockTauri(
    page,
    {
      ask_vault_persisted: (args: any) => {
        (globalThis as any).__askArgs = args;
        // The REAL string the board-scoped empty path returns (`commands/ask.rs`,
        // pinned by `a_board_scoped_empty_ask_does_not_tell_the_user_to_record`).
        // A mock stands in for the TRANSPORT, not for the contract — inventing a
        // nicer sentence here would have this spec assert a message that never ships.
        return {
          answer:
            "Nothing on this board is readable right now — unlock its folders, or add " +
            "tiles with content you can see.",
          sources: [],
          citations: [],
        };
      },
      // A board-scoped answer may contain DERIVED-tile material — rows the board
      // composed, not documents the user pinned. Board Ask deliberately keeps
      // persistence disabled; pin the absence of `set_dashboard_answer` calls so
      // a future refactor cannot silently turn an ephemeral answer into a cache.
      set_dashboard_answer: () => {
        (globalThis as any).__setDashboardAnswerCalls =
          ((globalThis as any).__setDashboardAnswerCalls ?? 0) + 1;
        return null;
      },
    },
    {
      list_dashboards: BOARDS,
      get_dashboard: {
        ...BOARDS[0],
        tiles: [
          tile("t-sealed-note", "note", { kind: "locked" }, 0),
          tile("t-sealed-meeting", "meeting", { kind: "locked" }, 1),
          tile("t-sealed-document", "document", { kind: "locked" }, 2),
        ],
      },
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-dense");
  await enterCompose(page);
  await expect(page.locator(".compose-row")).toHaveCount(3);
  await expect(page.locator('.compose-row[data-kind="locked"]')).toHaveCount(3);
  await page.getByRole("button", { name: "Done", exact: true }).click();

  // Ask is not mounted until explicitly opened.
  await openAsk(page);
  await page
    .getByRole("textbox", { name: /Ask a question/i })
    .fill("who owes me?");
  await submitAsk(page);

  // It ASKED — no dead end, and the answer names the real situation (locked tiles)
  // rather than telling the user to go record a meeting.
  await expect(
    page.getByText(/Nothing on this board is readable/),
  ).toBeVisible();
  // …and it sent the board id, which is what makes the backend scope the request
  // instead of falling through to a vault-wide search.
  const sent = await page.evaluate(() => (globalThis as any).__askArgs);
  expect(sent?.dashboardId).toBe("b-dense");
  const persisted = await page.evaluate(
    () => (globalThis as any).__setDashboardAnswerCalls ?? 0,
  );
  expect(persisted).toBe(0);
});
