import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * ONE rhythm for the whole sidebar.
 *
 * Three families of row are drawn by three different files — the destination
 * rows (`.sb-row`, styles.css), the workspace/shared trees
 * (`<mur-tree-row>`), and the "View all" continuation (`.see-all`) — and they
 * had drifted apart. Measured in the running app on 2026-09-14, BEFORE the fix:
 *
 *   family                    row height   pitch   label x
 *   mur-tree-row (depth 0)        34px    37-40px    65px
 *   mur-tree-row (depth 1)        34px      37px     75px
 *   .sb-row (Browse)              48px      50px     56px
 *   .sb-row (Meetings, Notes…)    48px      50px     81px
 *   .see-all                      25px        --       --
 *
 * Three heights, four label origins, and a disclosure caret on the LEFT in the
 * tree against the RIGHT in Browse — which is what made one column read as
 * several unrelated widgets stacked.
 *
 * These are the oracles for the fix, and they are what makes the shared
 * `--sidebar-*` tokens load-bearing rather than decorative: a value edited back
 * into one of the three files fails here. Every assertion below fails on the
 * numbers in that table, so none of them is vacuous.
 */
const NOTE_ROOT = {
  id: "f-notes-root",
  name: "Notes",
  kind: "note",
  level: "folder",
  emoji: null,
  tint: null,
  locked: false,
  unlocked: false,
  isRoot: true,
  folders: [],
  // total > items.length is what renders the "View all" continuation row.
  groups: [
    {
      kind: "note",
      total: 12,
      items: [
        { kind: "note", id: "n-1", title: "Loose thought", durationS: null, sortAt: 90 },
        { kind: "note", id: "n-2", title: "Second thought", durationS: null, sortAt: 89 },
      ],
    },
  ],
};

const FOREST = [
  {
    id: "p-acme",
    name: "Acme",
    kind: "note",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [NOTE_ROOT],
    groups: [],
  },
];

/** Boot with a tree AND Browse open, so all three row families are on screen. */
async function boot(page: Page): Promise<void> {
  await mockTauri(page, {}, { list_workspace_tree: FOREST });
  await page.goto("/record");
  await expect(
    page.getByRole("navigation", { name: "Primary navigation" }),
  ).toBeVisible();
  // The project has to be open for the note root's inbox — and the "View all"
  // continuation under it — to be on screen at all.
  await page
    .getByRole("treeitem", { name: /Acme/ })
    .getByRole("button", { name: /Expand/ })
    .click();
  const browse = page.locator(".sb-browse .sb-disclosure");
  await expect(browse).toBeVisible();
  if ((await browse.getAttribute("aria-expanded")) !== "true") {
    await browse.click();
  }
  await expect(page.locator(".sb-sublist")).toBeVisible();
  await expect(page.getByRole("treeitem", { name: /Unfiled notes/ })).toBeVisible();
}

/**
 * Geometry of every VISIBLE row, read from the live layout.
 *
 * The label origin is measured as where the TEXT actually starts (a Range over
 * the label's contents), not as an offsetLeft chain plus padding: the rows
 * carry a 1px transparent border for the trees' drop-target states, and an
 * offset-based read silently drops it — which reports a row as 1px out when it
 * is in fact aligned. `depth` is the nesting level a row is drawn at, so rows
 * that should share an origin are compared only with each other.
 */
async function rowGeometry(page: Page) {
  return page.evaluate(() => {
    const sidebar = document.querySelector(".primary-sidebar");
    if (!sidebar) throw new Error("no sidebar");
    const sidebarLeft = sidebar.getBoundingClientRect().left;
    const textX = (el: Element): number => {
      const range = document.createRange();
      range.selectNodeContents(el);
      return Math.round(range.getBoundingClientRect().left - sidebarLeft);
    };
    const rows: { kind: string; depth: number; height: number; labelX: number }[] = [];
    sidebar
      .querySelectorAll<HTMLElement>(".sb-row, mur-tree-row, .see-all")
      .forEach((el) => {
        // offsetParent is null for a row hidden by a collapsed ancestor.
        if (!el.offsetParent) return;
        const nested =
          el.closest(".sb-sublist") !== null ||
          Number(el.getAttribute("aria-level") ?? "1") > 1;
        rows.push({
          kind:
            el.tagName === "MUR-TREE-ROW"
              ? "tree"
              : el.className.includes("see-all")
                ? "seeAll"
                : "sbRow",
          depth: nested ? 1 : 0,
          height: el.offsetHeight,
          // The continuation row has no label element — its own text is the label.
          labelX: textX(el.querySelector(".sb-label, .row-label") ?? el),
        });
      });
    return rows;
  });
}

/** The type of a label, as the browser actually resolved it. */
async function typography(page: Page, selector: string) {
  return page.evaluate((sel) => {
    return [...document.querySelectorAll<HTMLElement>(sel)]
      .filter((el) => el.offsetParent)
      .map((el) => {
        const cs = getComputedStyle(el);
        return `${cs.fontSize}/${cs.fontWeight}/${cs.letterSpacing}`;
      });
  }, selector);
}

test("every sidebar row is the same height, whichever component draws it", async ({
  page,
}) => {
  await boot(page);
  const rows = await rowGeometry(page);
  // Guard against a vacuous pass: all three families must actually be present,
  // or "they all agree" would be a statement about one of them.
  expect([...new Set(rows.map((r) => r.kind))].sort()).toEqual([
    "sbRow",
    "seeAll",
    "tree",
  ]);
  const heights = [...new Set(rows.map((r) => r.height))];
  expect(heights, `one row height across ${rows.length} rows`).toHaveLength(1);
});

test("rows at the same depth start their label at the same x", async ({
  page,
}) => {
  await boot(page);
  const rows = await rowGeometry(page);
  for (const depth of [0, 1]) {
    const atDepth = rows.filter((r) => r.depth === depth);
    // Both a tree row and a .sb-row exist at each depth — the pairing that was
    // 9px out at depth 0 and 6px out at depth 1 before the fix.
    expect([...new Set(atDepth.map((r) => r.kind))].length).toBeGreaterThan(1);
    const origins = [...new Set(atDepth.map((r) => r.labelX))];
    expect(origins, `one label origin at depth ${depth}`).toHaveLength(1);
  }
});

test("every row label is set in the same type", async ({ page }) => {
  await boot(page);
  // Scoped to ROWS: the footer's Capture button also carries a `.sb-label`, and
  // it is a button in a button's type, not a row that has drifted.
  const labels = await typography(
    page,
    ".sb-row .sb-label, mur-tree-row .row-label",
  );
  expect(labels.length).toBeGreaterThan(4);
  expect([...new Set(labels)], "one row label type").toHaveLength(1);
});

test("every section header is set in the same type", async ({ page }) => {
  await boot(page);
  // "Workspaces"/"Shared" (.sb-section-title) and "Work"/"Intelligence"
  // (.sb-group-label) look like the same label and must BE the same label.
  const heads = await typography(
    page,
    ".primary-sidebar .sb-section-title, .primary-sidebar .sb-group-label",
  );
  expect(heads.length).toBeGreaterThan(2);
  expect([...new Set(heads)], "one section header type").toHaveLength(1);
});
