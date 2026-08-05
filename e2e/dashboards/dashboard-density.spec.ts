import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Open the Ask column. It starts collapsed to a rail — the scope count stays on
 * screen, the empty transcript does not — so any test that types a question expands
 * it first, which is the same click a user makes.
 */
async function openAsk(page: import("@playwright/test").Page): Promise<void> {
  // Wait for the panel to EXIST before deciding. A bare `count()` races Angular's
  // first render and silently no-ops, which then surfaces as a `fill` timeout on a
  // field that was never revealed.
  const panel = page.locator("aside.ask");
  await panel.waitFor();
  const rail = panel.locator("button.ask-rail");
  if (await rail.count()) await rail.click();
  await page.getByRole("textbox", { name: /Ask a question/i }).waitFor();
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
    tile("t-numbers", "numbers", { kind: "numbers", entity: "Brain", rows: [] }, 0),
    // Genuinely zero → GOOD NEWS → must stay a full tile.
    tile("t-promises", "promises", { kind: "promises", owner: null, rows: [] }, 1),
    // Populated stat cluster → narrow.
    tile(
      "t-person",
      "person",
      { kind: "person", id: "e-1", name: "Kuba", mentionCount: 12, openCommitments: 3 },
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
    { list_dashboards: BOARDS, get_dashboard: BOARD_DETAIL, get_dashboard_sources: [] },
  );
  await page.goto("/dashboards/b-dense");
  await expect(page.locator("app-dashboard-tile")).toHaveCount(4);
}

/** The rendered column span, read off the resolved grid placement. */
async function spanOf(page: import("@playwright/test").Page, id: string): Promise<number> {
  return page.evaluate((tileId) => {
    const el = document.querySelector(`app-dashboard-tile[data-tile-id="${tileId}"]`);
    if (!el) return -1;
    const raw = getComputedStyle(el).gridColumn; // e.g. "span 3 / auto"
    const m = /span\s+(\d+)/.exec(raw);
    return m ? Number(m[1]) : -1;
  }, id);
}

test("Dashboards: a tile that never had data collapses instead of reserving a full card", async ({
  page,
}) => {
  await openBoard(page);

  const numbers = page.locator('app-dashboard-tile[data-tile-id="t-numbers"]');
  await expect(numbers).toHaveClass(/is-empty/);

  // The apology is gone: no body region at all, and the strip is narrow.
  await expect(numbers.locator(".tile-body")).toHaveCount(0);
  expect(await spanOf(page, "t-numbers")).toBe(3);

  // And it is SHORT — the whole point. The invariant that matters is RELATIVE:
  // a collapsed strip must not read as a peer of a populated card. (An absolute
  // pixel ceiling would just encode today's font stack; the ratio is the design.)
  const heights = await page.evaluate(() => {
    const h = (id: string) =>
      document
        .querySelector(`app-dashboard-tile[data-tile-id="${id}"]`)!
        .getBoundingClientRect().height;
    return { empty: h("t-numbers"), full: h("t-note") };
  });
  expect(heights.empty).toBeLessThan(heights.full * 0.75);
  expect(heights.empty).toBeLessThanOrEqual(56);
});

test("Dashboards: an empty PROMISES tile reads as a result, not as an absence", async ({
  page,
}) => {
  await openBoard(page);

  const promises = page.locator('app-dashboard-tile[data-tile-id="t-promises"]');
  // Zero open commitments is good news — it keeps its card and its body.
  await expect(promises).not.toHaveClass(/is-empty/);
  await expect(promises.locator(".tile-body")).toHaveCount(1);
  await expect(promises.locator(".state-good")).toBeVisible();
});

test("Dashboards: kinds get different widths even though every stored row says span 4", async ({
  page,
}) => {
  await openBoard(page);

  const person = await spanOf(page, "t-person");
  const note = await spanOf(page, "t-note");
  const promises = await spanOf(page, "t-promises");

  // A stat cluster is narrower than prose, which is narrower than a ledger.
  expect(person).toBe(3);
  expect(note).toBe(4);
  expect(promises).toBe(6);
  expect(new Set([person, note, promises]).size).toBeGreaterThan(1);
});

test("Dashboards: every tile header carries a per-kind mark", async ({ page }) => {
  await openBoard(page);

  // The single biggest visual omission versus the prototype: the shipped header
  // was an <h3> and a grey uppercase word, with zero colour anywhere on the board.
  await expect(page.locator("app-dashboard-tile .tile-mark")).toHaveCount(4);
  await expect(
    page.locator('app-dashboard-tile[data-tile-id="t-note"] .tile-mark'),
  ).toHaveAttribute("data-kind", "note");
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
      ask_vault: () => {
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
  await page.getByRole("textbox", { name: /Ask a question/i }).fill("what is late?");
  await page.getByRole("button", { name: "Ask", exact: true }).click();

  const banner = page.locator(".thread .banner.is-danger");
  await expect(banner).toBeVisible();
  await expect(banner.getByRole("button", { name: /Try again/i })).toBeVisible();
  // And it is NOT dressed as an answer.
  await expect(page.locator(".thread .bubble:not(.me)")).toHaveCount(0);
});

test("Dashboards: an answer renders as markdown, not as literal asterisks", async ({ page }) => {
  // `summarize/vault_chat.rs::build` DEMANDS markdown from the model, and the
  // board rendered `{{ turn.text }}` with `white-space: pre-wrap`.
  await mockTauri(
    page,
    {
      ask_vault: () => ({
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
  await page.getByRole("textbox", { name: /Ask a question/i }).fill("what is the risk?");
  await page.getByRole("button", { name: "Ask", exact: true }).click();

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
              rows: [{ text: "Send the Acme paperwork", meta: "due 2026-07-22", status: "late", source: null }],
            },
            0,
          ),
          tile(
            "t-p2",
            "promises",
            {
              kind: "promises",
              owner: null,
              rows: [{ text: "Send the Acme paperwork", meta: "due 2026-07-22", status: "late", source: null }],
            },
            1,
          ),
        ],
      },
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/b-dense");
  await expect(page.locator("app-dashboard-tile")).toHaveCount(2);

  const second = page.locator('app-dashboard-tile[data-tile-id="t-p2"]');
  await expect(second).toHaveClass(/is-duplicate/);
  await expect(second.getByText(/same as the tile above/i)).toBeVisible();

  // The row is rendered ONCE on the board, not twice.
  await expect(page.getByText("Send the Acme paperwork")).toHaveCount(1);
});

test("Dashboards: the Ask column is a rail until it has something to say", async ({ page }) => {
  // The scope readout is the feature's claim made visible, so it stays on screen; the
  // empty transcript is not, and it was spending a third of the width on three
  // suggestion buttons.
  await mockTauri(
    page,
    { ask_vault: () => ({ answer: "Two are late.", sources: [], citations: [] }) },
    {
      list_dashboards: BOARDS,
      get_dashboard: BOARD_DETAIL,
      get_dashboard_sources: [{ kind: "note", id: "n-1" }],
    },
  );

  await page.goto("/dashboards/b-dense");
  const ask = page.locator("aside.ask");
  await expect(ask).toHaveClass(/is-rail/);
  // …and the count is still readable while collapsed.
  await expect(ask.getByText(/in scope/)).toBeVisible();
  const railWidth = (await ask.boundingBox())!.width;
  expect(railWidth).toBeLessThan(60);

  await ask.getByRole("button", { name: /Ask this board/ }).click();
  await expect(ask).not.toHaveClass(/is-rail/);
  // The width TRANSITIONS, so a single read lands mid-animation. Poll instead of
  // measuring once — and the composer being reachable is the real invariant anyway.
  await expect(page.getByRole("textbox", { name: /Ask a question/i })).toBeVisible();
  await expect
    .poll(async () => (await ask.boundingBox())!.width)
    .toBeGreaterThan(railWidth * 3);

  // Once there is a conversation it STAYS open — a thread you cannot see is worse
  // than a wide column.
  await openAsk(page);
  await page.getByRole("textbox", { name: /Ask a question/i }).fill("who is late?");
  await page.getByRole("button", { name: "Ask", exact: true }).click();
  await expect(page.getByText("Two are late.")).toBeVisible();
  await expect(ask).not.toHaveClass(/is-rail/);
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

test("Dashboards: renaming with no emoji clears the old one", async ({ page }) => {
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
        (globalThis as any).__deleted = ((globalThis as any).__deleted ?? 0) + 1;
        return true;
      },
    },
    { list_dashboards: BOARDS, get_dashboard: BOARD_DETAIL, get_dashboard_sources: [] },
  );

  await page.goto("/dashboards");
  const card = page.locator(".board-card").first();

  // FIRST click arms — it must not delete.
  await card.getByRole("button", { name: /^Delete / }).click();
  await expect(card.getByRole("button", { name: /^Confirm delete / })).toBeVisible();
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
      ask_vault: async () => {
        await new Promise((r) => setTimeout(r, 1500));
        return { answer: "answer for the first board", sources: [], citations: [] };
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
  await page.getByRole("textbox", { name: /Ask a question/i }).fill("slow question");
  await page.getByRole("button", { name: "Ask", exact: true }).click();

  // Leave while it is still running, then come back.
  await page.goto("/dashboards");
  await page.goto("/dashboards/b-dense");
  await openAsk(page);

  // The Ask control is USABLE — not stuck behind a dead request…
  await page.getByRole("textbox", { name: /Ask a question/i }).fill("a fresh question");
  await expect(page.getByRole("button", { name: "Ask", exact: true })).toBeEnabled();
  // …and the previous board's thread did not follow us here.
  await expect(page.getByText("slow question")).toHaveCount(0);
  await expect(page.getByText("answer for the first board")).toHaveCount(0);
});
