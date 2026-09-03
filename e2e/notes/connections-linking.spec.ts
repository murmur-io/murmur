import { test, expect, type Page } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

const ORG_ID = "11111111-1111-4111-8111-111111111111";
const OTHER_ORG_ID = "22222222-2222-4222-8222-222222222222";
const DOC_OUT_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const DOC_IN_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const ORG_OUT_LINK_ID = `${ORG_ID}:${DOC_OUT_ID}`;
const ORG_IN_LINK_ID = `${ORG_ID}:${DOC_IN_ID}`;

async function emitOrgFeedUpdated(page: Page): Promise<void> {
  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://org-feed-updated", { orgsChanged: 1 });
  });
}

/**
 * PR-1 — user-initiated linking UI in the shared `app-connections` panel (mounted
 * in the note editor at /notes/n1). Verifies, against a mocked Tauri IPC:
 *  - `list_links` renders a mix of chips; ONLY the `manual:true` chip shows a `×`.
 *  - clicking that `×` invokes `unlink_items` with the anchor→neighbour args and
 *    then re-fetches `list_links` (the chip drops out).
 *  - the `+ Link` chooser opens the OPAQUE hierarchy modal, and picking a leaf
 *    invokes `link_items(anchorKind, anchorId, kind, id)`
 *    then re-fetches (the new chip appears).
 *
 * The mock records every link_items/unlink_items call on `window.__linkCalls` so the
 * test can assert the exact args (the overrides are serialized page-side — no test
 * closures — so they stash calls on `window` and drive `list_links` off a flag they
 * also set on `window`). The core gate is ZERO console/page errors through the flow.
 */
test("connections panel: × only on manual chips (unlink), + Link chooser links a candidate — no console errors", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, {
    // list_links: three chips before any mutation — a MANUAL (removable) wikilink,
    // an AUTO companion (not removable), and a SEMANTIC suggestion. After an unlink
    // or a link the mock flips window.__linksAfter so the re-fetch returns a fresh set.
    list_links: () => {
      const w = window as unknown as {
        __linksAfterUnlink?: boolean;
        __linksAfterLink?: boolean;
      };
      const rows = [
        // The manual (removable) chip — dropped once __linksAfterUnlink flips.
        {
          id: 1,
          direction: "out",
          otherKind: "meeting",
          otherId: "m-manual",
          otherTitle: "Manual Meeting Link",
          edgeType: "wikilink",
          createdBy: "user",
          status: "active",
          score: 1.0,
          createdAt: 1_720_000_100,
          manual: true,
        },
        {
          id: 2,
          direction: "out",
          otherKind: "note",
          otherId: "n2",
          otherTitle: "Weekly plan",
          edgeType: "companion",
          createdBy: "auto",
          status: "active",
          score: 1.0,
          createdAt: 1_720_000_000,
          manual: false,
        },
        {
          id: 3,
          direction: "in",
          otherKind: "note",
          otherId: "n-sugg",
          otherTitle: "A suggested note",
          edgeType: "semantic",
          createdBy: "auto",
          status: "suggested",
          score: 0.86,
          createdAt: 1_720_000_050,
          manual: false,
        },
      ];
      const out = w.__linksAfterUnlink
        ? rows.filter((r) => r.id !== 1) // manual chip removed by unlink
        : rows;
      if (w.__linksAfterLink) {
        out.push({
          id: 9,
          direction: "out",
          otherKind: "meeting",
          otherId: "m-cand",
          otherTitle: "Some meeting",
          edgeType: "manual",
          createdBy: "user",
          status: "active",
          score: 1.0,
          createdAt: 1_720_000_200,
          manual: true,
        });
      }
      return out;
    },

    // Record link/unlink calls + flip the re-fetch flag so list_links changes.
    link_items: (args: unknown) => {
      const w = window as unknown as {
        __linkCalls?: unknown[];
        __linksAfterLink?: boolean;
      };
      (w.__linkCalls = w.__linkCalls || []).push({ cmd: "link_items", args });
      w.__linksAfterLink = true;
      return null;
    },
    unlink_items: (args: unknown) => {
      const w = window as unknown as {
        __linkCalls?: unknown[];
        __linksAfterUnlink?: boolean;
      };
      (w.__linkCalls = w.__linkCalls || []).push({ cmd: "unlink_items", args });
      w.__linksAfterUnlink = true;
      return null;
    },

    // Keep backlinks empty so only the connections panel is under test.
    get_backlinks: () => [],
  });

  await page.goto("/notes/n1");

  const panel = page.locator("app-connections");
  await expect(panel).toBeVisible();

  // Existing relationships stay collapsed by default, but linking is an
  // independent command: it must remain reachable without revealing the rows.
  const collapsed = panel.getByRole("button", {
    name: "Show related items and suggestions",
  });
  await expect(collapsed).toHaveAttribute("aria-expanded", "false");
  const collapsedLink = panel.getByRole("button", {
    name: "Link",
    exact: true,
  });
  await expect(collapsedLink).toBeVisible();
  await collapsedLink.click();
  await expect(page.getByRole("dialog", { name: "Add related" })).toBeVisible();
  await expect(page.getByPlaceholder("Search every Space…")).toBeFocused();
  await expect(collapsed).toHaveAttribute("aria-expanded", "false");
  await expect(panel.locator(".cx-group")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(collapsedLink).toBeVisible();

  // Expanding is only for inspecting/managing the relationship rows.
  await collapsed.click();

  // Three chips render in the merged "Related" panel: the manual (removable) link,
  // the auto companion link, and the semantic suggestion (an ambient dashed chip).
  await expect(panel.getByText("Manual Meeting Link")).toBeVisible();
  await expect(panel.getByText("Weekly plan")).toBeVisible();
  await expect(panel.getByText("A suggested note")).toBeVisible();

  // The MANUAL deterministic chip carries a "Remove link to …" × (unlink); the
  // semantic suggestion carries a "Dismiss suggestion …" × instead (2026-07-19 IA:
  // suggestions are ambient dashed chips — tap promotes, hover × dismisses — no
  // persistent Accept/Dismiss buttons). Target by aria-label, not a bare .cx-remove
  // count (both kinds now use the same hover-× visual).
  const unlinkBtn = panel.getByRole("button", {
    name: "Remove link to Manual Meeting Link",
  });
  await expect(unlinkBtn).toBeVisible();

  // Click the × → unlink_items(anchorKind=note, anchorId=n1, otherKind=meeting,
  // otherId=m-manual) is invoked, then the re-fetch drops the manual chip.
  await unlinkBtn.click();
  await expect(panel.getByText("Manual Meeting Link")).toHaveCount(0);

  const afterUnlink = await page.evaluate(
    () => (window as unknown as { __linkCalls?: unknown[] }).__linkCalls || [],
  );
  expect(afterUnlink).toEqual([
    {
      cmd: "unlink_items",
      args: {
        srcKind: "note",
        srcId: "n1",
        dstKind: "meeting",
        dstId: "m-manual",
      },
    },
  ]);

  // Open the + Link chooser → the opaque hierarchy modal appears.
  // `exact` so the substring name doesn't also match a suggestion chip's
  // "Add link to …" aria-label (2026-07-19 ambient suggestion chips are buttons).
  await panel.getByRole("button", { name: "Link", exact: true }).click();
  const picker = page.getByRole("dialog", { name: "Add related" });
  await expect(picker).toBeVisible();

  // The anchor's note group opens automatically. Expand the sibling recordings
  // group, then link a leaf immediately.
  await picker.getByRole("button", { name: "Expand Recordings" }).click();
  const pickedRow = picker.getByRole("button", {
    name: "Some meeting",
    exact: true,
  });
  await expect(pickedRow).toBeVisible();
  await pickedRow.click();

  // The new manual chip appears from the re-fetch; link_items was called correctly.
  await expect(panel.getByText("Some meeting")).toBeVisible();
  const afterLink = await page.evaluate(
    () => (window as unknown as { __linkCalls?: unknown[] }).__linkCalls || [],
  );
  expect(afterLink).toContainEqual({
    cmd: "link_items",
    args: { srcKind: "note", srcId: "n1", dstKind: "meeting", dstId: "m-cand" },
  });

  expect(consoleErrors).toEqual([]);
});

/**
 * Directed-edge regression: `list_links` returns every edge incident on the
 * anchor, but `unlink_items` deletes one exact stored `(src, dst)` tuple. The UI
 * therefore has to preserve `LinkEdge.direction`: an incoming chip represents
 * `other → anchor`, while an outgoing chip represents `anchor → other`.
 *
 * This mock is deliberately stateful and backend-shaped. It removes a tuple only
 * when all four endpoint fields match, then derives the next `list_links` reply
 * from the remaining tuples. An unconditional "after unlink" flag would let the
 * reverse-direction bug pass while the real Rust command leaves the chip intact.
 */
test("unlink preserves the exact directed tuple for outgoing and incoming links", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: (args: { kind: string; id: string }) => {
      const w = window as unknown as {
        __directedLinks?: {
          id: number;
          srcKind: string;
          srcId: string;
          srcTitle: string;
          dstKind: string;
          dstId: string;
          dstTitle: string;
        }[];
      };
      w.__directedLinks ??= [
        {
          id: 101,
          srcKind: "note",
          srcId: "n1",
          srcTitle: "My First Note",
          dstKind: "meeting",
          dstId: "m-out",
          dstTitle: "Outgoing meeting",
        },
        {
          id: 102,
          srcKind: "org",
          srcId:
            "11111111-1111-4111-8111-111111111111:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
          srcTitle: "Incoming shared note",
          dstKind: "note",
          dstId: "n1",
          dstTitle: "My First Note",
        },
      ];
      return w.__directedLinks
        .filter(
          (edge) =>
            (edge.srcKind === args.kind && edge.srcId === args.id) ||
            (edge.dstKind === args.kind && edge.dstId === args.id),
        )
        .map((edge) => {
          const outgoing = edge.srcKind === args.kind && edge.srcId === args.id;
          return {
            id: edge.id,
            direction: outgoing ? "out" : "in",
            otherKind: outgoing ? edge.dstKind : edge.srcKind,
            otherId: outgoing ? edge.dstId : edge.srcId,
            otherTitle: outgoing ? edge.dstTitle : edge.srcTitle,
            edgeType: "manual",
            createdBy: "user",
            status: "active",
            score: 1,
            createdAt: 1_720_000_400 + edge.id,
            manual: true,
          };
        });
    },
    unlink_items: (args: {
      srcKind: string;
      srcId: string;
      dstKind: string;
      dstId: string;
    }) => {
      const w = window as unknown as {
        __directedLinks?: {
          srcKind: string;
          srcId: string;
          dstKind: string;
          dstId: string;
        }[];
        __directedUnlinkCalls?: unknown[];
      };
      (w.__directedUnlinkCalls ??= []).push(args);
      const exactIndex = (w.__directedLinks ?? []).findIndex(
        (edge) =>
          edge.srcKind === args.srcKind &&
          edge.srcId === args.srcId &&
          edge.dstKind === args.dstKind &&
          edge.dstId === args.dstId,
      );
      if (exactIndex >= 0) {
        w.__directedLinks!.splice(exactIndex, 1);
      }
      return null;
    },
    get_backlinks: () => [],
  });

  await page.goto("/notes/n1");
  const panel = page.locator("app-connections");
  await panel
    .getByRole("button", { name: "Show related items and suggestions" })
    .click();

  await expect(
    panel.getByText("Outgoing meeting", { exact: true }),
  ).toBeVisible();
  await expect(
    panel.getByText("Incoming shared note", { exact: true }),
  ).toBeVisible();

  await panel
    .getByRole("button", { name: "Remove link to Outgoing meeting" })
    .click();
  await expect(
    panel.getByText("Outgoing meeting", { exact: true }),
  ).toHaveCount(0);
  await expect(
    panel.getByText("Incoming shared note", { exact: true }),
  ).toBeVisible();

  await panel
    .getByRole("button", { name: "Remove link to Incoming shared note" })
    .click();
  await expect(
    panel.getByText("Incoming shared note", { exact: true }),
  ).toHaveCount(0);

  const calls = await page.evaluate(
    () =>
      (window as unknown as { __directedUnlinkCalls?: unknown[] })
        .__directedUnlinkCalls ?? [],
  );
  expect(calls).toEqual([
    {
      srcKind: "note",
      srcId: "n1",
      dstKind: "meeting",
      dstId: "m-out",
    },
    {
      srcKind: "org",
      srcId: ORG_IN_LINK_ID,
      dstKind: "note",
      dstId: "n1",
    },
  ]);
});

test("collapsed opposite manual tuple is removed while the wikilink survives", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => {
      const w = window as unknown as {
        __oppositeManual?: {
          srcKind: "note";
          srcId: string;
          dstKind: "note";
          dstId: string;
        }[];
      };
      w.__oppositeManual ??= [
        {
          srcKind: "note",
          srcId: "n-target",
          dstKind: "note",
          dstId: "n1",
        },
      ];
      return [
        {
          id: 301,
          direction: "out",
          otherKind: "note",
          otherId: "n-target",
          otherTitle: "Opposite target",
          edgeType: "wikilink",
          createdBy: "auto",
          status: "active",
          score: 1,
          createdAt: 1_720_000_301,
          manual: w.__oppositeManual.length > 0,
          manualEdges: [...w.__oppositeManual],
        },
      ];
    },
    unlink_items: (args: {
      srcKind: string;
      srcId: string;
      dstKind: string;
      dstId: string;
      manualEdges?: {
        srcKind: "note";
        srcId: string;
        dstKind: "note";
        dstId: string;
      }[];
    }) => {
      const w = window as unknown as {
        __oppositeManual?: typeof args.manualEdges;
        __oppositeUnlink?: typeof args;
      };
      w.__oppositeUnlink = args;
      const exact = args.manualEdges ?? [];
      w.__oppositeManual = (w.__oppositeManual ?? []).filter(
        (stored) =>
          !exact.some(
            (edge) =>
              edge.srcKind === stored.srcKind &&
              edge.srcId === stored.srcId &&
              edge.dstKind === stored.dstKind &&
              edge.dstId === stored.dstId,
          ),
      );
      return null;
    },
    get_backlinks: () => [],
  });

  await page.goto("/notes/n1");
  const panel = page.locator("app-connections");
  await panel
    .getByRole("button", { name: "Show related items and suggestions" })
    .click();
  await panel
    .getByRole("button", { name: "Remove link to Opposite target" })
    .click();

  await expect(
    panel.getByText("Opposite target", { exact: true }),
  ).toBeVisible();
  await expect(
    panel.getByRole("button", { name: "Remove link to Opposite target" }),
  ).toHaveCount(0);
  const call = await page.evaluate(
    () =>
      (window as unknown as { __oppositeUnlink?: unknown }).__oppositeUnlink,
  );
  expect(call).toEqual({
    srcKind: "note",
    srcId: "n1",
    dstKind: "note",
    dstId: "n-target",
    manualEdges: [
      {
        srcKind: "note",
        srcId: "n-target",
        dstKind: "note",
        dstId: "n1",
      },
    ],
  });
});

test("one collapsed unlink removes both directed manual tuples", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () => {
      const w = window as unknown as {
        __bidirectionalManual?: {
          srcKind: "note";
          srcId: string;
          dstKind: "note";
          dstId: string;
        }[];
      };
      w.__bidirectionalManual ??= [
        {
          srcKind: "note",
          srcId: "n1",
          dstKind: "note",
          dstId: "n-both",
        },
        {
          srcKind: "note",
          srcId: "n-both",
          dstKind: "note",
          dstId: "n1",
        },
      ];
      return w.__bidirectionalManual.length
        ? [
            {
              id: 401,
              direction: "out",
              otherKind: "note",
              otherId: "n-both",
              otherTitle: "Both directions",
              edgeType: "manual",
              createdBy: "user",
              status: "active",
              score: 1,
              createdAt: 1_720_000_401,
              manual: true,
              manualEdges: [...w.__bidirectionalManual],
            },
          ]
        : [];
    },
    unlink_items: (args: {
      manualEdges?: {
        srcKind: "note";
        srcId: string;
        dstKind: "note";
        dstId: string;
      }[];
    }) => {
      const w = window as unknown as {
        __bidirectionalManual?: typeof args.manualEdges;
        __bidirectionalUnlink?: typeof args;
      };
      w.__bidirectionalUnlink = args;
      if ((args.manualEdges ?? []).length === 2) {
        w.__bidirectionalManual = [];
      }
      return null;
    },
    get_backlinks: () => [],
  });

  await page.goto("/notes/n1");
  const panel = page.locator("app-connections");
  await panel
    .getByRole("button", { name: "Show related items and suggestions" })
    .click();
  await panel
    .getByRole("button", { name: "Remove link to Both directions" })
    .click();
  await expect(panel.getByText("Both directions", { exact: true })).toHaveCount(
    0,
  );

  const call = await page.evaluate(
    () =>
      (
        window as unknown as {
          __bidirectionalUnlink?: { manualEdges?: unknown[] };
        }
      ).__bidirectionalUnlink,
  );
  expect(call?.manualEdges).toEqual([
    {
      srcKind: "note",
      srcId: "n1",
      dstKind: "note",
      dstId: "n-both",
    },
    {
      srcKind: "note",
      srcId: "n-both",
      dstKind: "note",
      dstId: "n1",
    },
  ]);
});

test("Shared Brain chip follows its stable link to the current live revision", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: (args: { kind: string }) =>
      args.kind === "note"
        ? [
            {
              id: 77,
              direction: "out",
              otherKind: "org",
              otherId:
                "11111111-1111-4111-8111-111111111111:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
              navigationId: "item-current-9",
              otherTitle: "Shared roadmap",
              edgeType: "manual",
              createdBy: "user",
              status: "active",
              score: 1,
              createdAt: 1_720_000_200,
              manual: true,
            },
          ]
        : [],
    get_backlinks: () => [],
    org_resolve_source: () => null,
    org_get_item: () => ({
      itemId: "item-current-9",
      docId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      linkId:
        "11111111-1111-4111-8111-111111111111:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      authorHint: "kasia",
      title: "Shared roadmap",
      createdAt: "2026-08-12T10:00:00Z",
      rev: 9,
      markdown: "# Current revision",
      access: "view",
      canEdit: false,
      canManage: false,
    }),
    org_list_statuses: () => [],
    list_note_attachments: () => [],
    account_status: () => ({ loggedIn: true }),
  });

  await page.goto("/notes/n1");
  const panel = page.locator("app-connections");
  await panel
    .getByRole("button", { name: "Show related items and suggestions" })
    .click();
  await panel
    .getByRole("button", { name: "Open Shared Brain item Shared roadmap" })
    .click();
  await expect(page).toHaveURL(/\/org-item\/item-current-9$/);
  await expect(page.getByText("revision 9")).toBeVisible();
});

test("org-anchored hierarchy keeps same-org and cross-org stable targets and marks exact self Current", async ({
  page,
}) => {
  await mockNotes(page, {
    org_resolve_source: () => null,
    org_get_item: () => ({
      itemId: "item-anchor-r3",
      docId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      linkId:
        "11111111-1111-4111-8111-111111111111:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      authorHint: "kasia",
      title: "Anchor roadmap",
      createdAt: "2026-08-12T10:00:00Z",
      rev: 3,
      markdown: "# Anchor roadmap",
      access: "edit",
      canEdit: true,
      canManage: false,
    }),
    list_links: () => [],
    list_shared_workspace: () => ({
      spaces: [
        {
          containerId: "shared-a",
          orgId: "11111111-1111-4111-8111-111111111111",
          orgName: "Team A",
          name: "Team A Space",
          level: "space",
          access: "view",
          authorHint: "kasia",
          folders: [],
          items: [
            {
              itemId: "self-r3",
              docId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
              title: "Anchor roadmap",
              kind: "document",
              authorHint: "kasia",
              createdAt: "2026-08-12T10:00:00Z",
              orgId: "11111111-1111-4111-8111-111111111111",
              orgName: "Team A",
              access: "view",
              position: 0,
            },
            {
              itemId: "same-r1",
              docId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
              title: "Same org neighbour",
              kind: "document",
              authorHint: "kasia",
              createdAt: "2026-08-12T10:00:00Z",
              orgId: "11111111-1111-4111-8111-111111111111",
              orgName: "Team A",
              access: "view",
              position: 1,
            },
          ],
          position: 0,
        },
        {
          containerId: "shared-b",
          orgId: "22222222-2222-4222-8222-222222222222",
          orgName: "Team B",
          name: "Team B Space",
          level: "space",
          access: "view",
          authorHint: "ola",
          folders: [],
          items: [
            {
              itemId: "cross-r1",
              docId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
              title: "Same document id in another org",
              kind: "document",
              authorHint: "ola",
              createdAt: "2026-08-12T10:00:00Z",
              orgId: "22222222-2222-4222-8222-222222222222",
              orgName: "Team B",
              access: "view",
              position: 0,
            },
          ],
          position: 1,
        },
      ],
      sharedBrains: {
        orgId: "shared",
        orgName: "Shared",
        name: "Shared Brains",
        level: "virtual",
        access: "view",
        authorHint: "",
        folders: [],
        items: [],
        position: 2,
      },
    }),
    link_items: (args: unknown) => {
      const target = window as unknown as { __orgToOrgLinkCalls?: unknown[] };
      (target.__orgToOrgLinkCalls ??= []).push(args);
      return null;
    },
    list_note_attachments: () => [],
    account_status: () => ({ loggedIn: true }),
  });

  await page.goto("/org-item/item-anchor-r3");
  const panel = page.locator("app-connections");
  await expect(panel).toBeVisible();

  await panel.getByRole("button", { name: "Link", exact: true }).click();
  const picker = page.getByRole("dialog", { name: "Add related" });
  await expect(
    picker.getByRole("button", { name: "Collapse Team A Space" }),
  ).toBeVisible();
  await picker.getByRole("button", { name: "Expand Team B Space" }).click();
  await expect(
    picker.getByText("Anchor roadmap", { exact: true }),
  ).toBeVisible();
  await expect(picker.getByText("Current", { exact: true })).toBeVisible();
  await expect(
    picker.locator('[data-row="si:same-r1"] .rhp-row-main'),
  ).toBeVisible();
  await expect(
    picker.locator('[data-row="si:cross-r1"] .rhp-row-main'),
  ).toBeVisible();

  await picker.locator('[data-row="si:same-r1"] .rhp-row-main').click();
  await panel.getByRole("button", { name: "Link", exact: true }).click();
  const secondPicker = page.getByRole("dialog", { name: "Add related" });
  await secondPicker
    .getByRole("button", { name: "Expand Team B Space" })
    .click();
  await secondPicker.locator('[data-row="si:cross-r1"] .rhp-row-main').click();

  const calls = await page.evaluate(
    () =>
      (window as unknown as { __orgToOrgLinkCalls?: unknown[] })
        .__orgToOrgLinkCalls ?? [],
  );
  expect(calls).toEqual([
    {
      srcKind: "org",
      srcId: ORG_OUT_LINK_ID,
      dstKind: "org",
      dstId: ORG_IN_LINK_ID,
    },
    {
      srcKind: "org",
      srcId: ORG_OUT_LINK_ID,
      dstKind: "org",
      dstId: `${OTHER_ORG_ID}:${DOC_OUT_ID}`,
    },
  ]);
});

test("org feed invalidates Connections immediately and converges revised, withdrawn, and stale neighbours", async ({
  page,
}) => {
  await mockNotes(page, {
    org_resolve_source: () => null,
    org_get_item: (args: { itemId: string }) => ({
      itemId: args.itemId,
      docId:
        args.itemId === "item-anchor-r1"
          ? "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
          : "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
      linkId:
        args.itemId === "item-anchor-r1"
          ? "11111111-1111-4111-8111-111111111111:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
          : "11111111-1111-4111-8111-111111111111:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
      authorHint: "kasia",
      title: args.itemId === "item-anchor-r1" ? "Anchor" : "Shared roadmap r4",
      createdAt: "2026-08-12T10:00:00Z",
      rev: args.itemId === "item-anchor-r1" ? 1 : 4,
      markdown: "# Shared item",
      access: "view",
      canEdit: false,
      canManage: false,
    }),
    list_links: () => {
      const target = window as unknown as {
        __orgLinkMode?: string;
        __resolveOrgLinkReload?: () => void;
      };
      const edge = (title: string, navigationId: string, id: number) => ({
        id,
        direction: "out",
        otherKind: "org",
        otherId:
          "11111111-1111-4111-8111-111111111111:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        navigationId,
        otherTitle: title,
        edgeType: "manual",
        createdBy: "user",
        status: "active",
        score: 1,
        createdAt: 1_720_000_000 + id,
        manual: true,
      });
      switch (target.__orgLinkMode) {
        case "revision-pending":
          return new Promise<unknown[]>((resolve) => {
            target.__resolveOrgLinkReload = () =>
              resolve([edge("Shared roadmap r2", "item-neighbour-r2", 702)]);
          });
        case "stale-pending":
          return new Promise<unknown[]>((resolve) => {
            target.__resolveOrgLinkReload = () =>
              resolve([edge("Stale roadmap r3", "item-neighbour-r3", 703)]);
          });
        case "withdrawn":
          return [];
        case "revived":
          return [edge("Shared roadmap r4", "item-neighbour-r4", 704)];
        default:
          return [edge("Shared roadmap r1", "item-neighbour-r1", 701)];
      }
    },
    org_refresh: () => null,
    org_list_statuses: () => [],
    list_meeting_org_shares: () => [],
    list_note_attachments: () => [],
    account_status: () => ({ loggedIn: true }),
  });

  await page.goto("/org-item/item-anchor-r1");
  const panel = page.locator("app-connections");
  await expect(
    panel.getByText("Shared roadmap r1", { exact: true }),
  ).toBeVisible();

  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as unknown as {
            __demoEventListenerRegistrationCount: (event: string) => number;
          }
        ).__demoEventListenerRegistrationCount("murmur://org-feed-updated"),
      ),
    )
    .toBeGreaterThanOrEqual(2);

  await panel.getByRole("button", { name: "Link", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Add related" })).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { __orgLinkMode?: string }).__orgLinkMode =
      "revision-pending";
  });
  await emitOrgFeedUpdated(page);

  // Invalidated synchronously, before the replacement `list_links` promise resolves.
  await expect(
    panel.getByText("Shared roadmap r1", { exact: true }),
  ).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Add related" })).toHaveCount(
    0,
  );
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (window as unknown as { __resolveOrgLinkReload?: unknown })
            .__resolveOrgLinkReload,
      ),
    )
    .toBe("function");
  await page.evaluate(() => {
    (
      window as unknown as { __resolveOrgLinkReload: () => void }
    ).__resolveOrgLinkReload();
  });
  await expect(
    panel.getByText("Shared roadmap r2", { exact: true }),
  ).toBeVisible();

  // A later withdrawal clears the revised chip. A still-later stale request cannot restore it.
  await page.evaluate(() => {
    (window as unknown as { __orgLinkMode?: string }).__orgLinkMode =
      "stale-pending";
  });
  await emitOrgFeedUpdated(page);
  await expect(
    panel.getByText("Shared roadmap r2", { exact: true }),
  ).toHaveCount(0);
  await page.evaluate(() => {
    (window as unknown as { __orgLinkMode?: string }).__orgLinkMode =
      "withdrawn";
  });
  await emitOrgFeedUpdated(page);
  await page.evaluate(() => {
    (
      window as unknown as { __resolveOrgLinkReload: () => void }
    ).__resolveOrgLinkReload();
  });
  await expect(
    panel.getByText("Stale roadmap r3", { exact: true }),
  ).toHaveCount(0);

  // A fresh live head refreshes both the title and the navigation item id.
  await page.evaluate(() => {
    (window as unknown as { __orgLinkMode?: string }).__orgLinkMode = "revived";
  });
  await emitOrgFeedUpdated(page);
  await expect(
    panel.getByText("Shared roadmap r4", { exact: true }),
  ).toBeVisible();
  await panel
    .getByRole("button", { name: "Open Shared Brain item Shared roadmap r4" })
    .click();
  await expect(page).toHaveURL(/\/org-item\/item-neighbour-r4$/);
});

/**
 * Regression for the empty-state trap: the panel used to be gated by
 * `hasAnything()`, which hid its only manual-link entry point precisely when a
 * note had no relationships. This drives the real empty DOM through the shared
 * picker and proves that creating the first link transitions to `Related · 1`.
 */
test("empty Related state keeps + Link reachable and creates the first relationship", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, {
    list_links: () => {
      const linked = (window as unknown as { __emptyLinked?: boolean })
        .__emptyLinked;
      return linked
        ? [
            {
              id: 91,
              direction: "out",
              otherKind: "note",
              otherId: "n-first",
              otherTitle: "First linked note",
              edgeType: "manual",
              createdBy: "user",
              status: "active",
              score: 1.0,
              createdAt: 1_720_000_300,
              manual: true,
            },
          ]
        : [];
    },
    get_backlinks: () => [],
    get_related_picker_bootstrap: (args: unknown) => {
      const w = window as unknown as {
        __emptyBootstrapCalls?: unknown[];
      };
      (w.__emptyBootstrapCalls ??= []).push(args);
      return {
        spaces: [
          {
            id: "p-root",
            name: "Workspace",
            level: "project",
            emoji: null,
            locked: false,
            unlocked: false,
            linkable: true,
            groups: [{ kind: "note", total: 2 }],
            folders: [],
          },
        ],
        unclassified: [],
        anchor: {
          kind: "note",
          containerId: "p-root",
          path: ["p-root"],
          index: 0,
          offset: 0,
          items: [
            { kind: "note", id: "n1", title: "My First Note" },
            { kind: "note", id: "n-first", title: "First linked note" },
          ],
          total: 2,
        },
      };
    },
    link_items: (args: unknown) => {
      const w = window as unknown as {
        __emptyLinkCalls?: unknown[];
        __emptyLinked?: boolean;
      };
      (w.__emptyLinkCalls ??= []).push(args);
      w.__emptyLinked = true;
      return null;
    },
  });

  await page.goto("/notes/n1");

  const panel = page.locator("app-connections");
  await expect(panel).toBeVisible();
  await expect(panel.locator(".cx--empty")).toBeVisible();
  await expect(panel.locator(".cx-collapsed")).toHaveCount(0);

  const linkTrigger = panel.getByRole("button", { name: "Link", exact: true });
  await expect(linkTrigger).toBeVisible();
  await linkTrigger.click();

  const picker = page.getByRole("dialog", { name: "Add related" });
  await expect(picker).toBeVisible();
  await expect(
    picker.getByText("My First Note", { exact: true }),
  ).toBeVisible();
  await expect(picker.getByText("Current", { exact: true })).toBeVisible();

  const firstTarget = picker.getByRole("button", {
    name: "First linked note",
    exact: true,
  });
  await expect(firstTarget).toBeVisible();
  await firstTarget.click();

  // The write re-fetches server truth: the empty trigger becomes the normal
  // collapsed Related row, and expanding it reveals the newly linked chip.
  const collapsed = panel.getByRole("button", {
    name: "Show related items and suggestions",
  });
  await expect(collapsed).toBeVisible();
  await expect(collapsed.locator(".cx-count")).toHaveText("1");
  await expect(
    panel.getByRole("button", { name: "Link", exact: true }),
  ).toBeVisible();
  await collapsed.click();
  await expect(
    panel.getByText("First linked note", { exact: true }),
  ).toBeVisible();

  const linkCalls = await page.evaluate(
    () =>
      (window as unknown as { __emptyLinkCalls?: unknown[] })
        .__emptyLinkCalls ?? [],
  );
  expect(linkCalls).toEqual([
    {
      srcKind: "note",
      srcId: "n1",
      dstKind: "note",
      dstId: "n-first",
    },
  ]);

  const bootstrapCalls = await page.evaluate(
    () =>
      (window as unknown as { __emptyBootstrapCalls?: unknown[] })
        .__emptyBootstrapCalls ?? [],
  );
  expect(bootstrapCalls).toContainEqual({ anchorKind: "note", anchorId: "n1" });
  expect(consoleErrors).toEqual([]);
});

/**
 * Lock negative: a masked note must not mount relationship UI or make any
 * relationship/candidate read. This pins the note-editor gate in addition to the
 * component's own `@if (!locked())` belt-and-braces guard.
 */
test("locked note hides Related and performs no relationship picker reads", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: (args: unknown) => {
      const w = window as unknown as { __lockedConnectionReads?: unknown[] };
      (w.__lockedConnectionReads ??= []).push({ cmd: "list_links", args });
      return [];
    },
    get_backlinks: (args: unknown) => {
      const w = window as unknown as { __lockedConnectionReads?: unknown[] };
      (w.__lockedConnectionReads ??= []).push({ cmd: "get_backlinks", args });
      return [];
    },
    get_related_picker_bootstrap: (args: unknown) => {
      const w = window as unknown as { __lockedConnectionReads?: unknown[] };
      (w.__lockedConnectionReads ??= []).push({
        cmd: "get_related_picker_bootstrap",
        args,
      });
      return { spaces: [], unclassified: [], anchor: null };
    },
  });

  await page.goto("/notes/nlk");

  await expect(page.getByText("This note is locked")).toBeVisible();
  await expect(page.locator("app-connections")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Link", exact: true }),
  ).toHaveCount(0);
  await expect(page.locator(".rhp")).toHaveCount(0);

  const reads = await page.evaluate(
    () =>
      (window as unknown as { __lockedConnectionReads?: unknown[] })
        .__lockedConnectionReads ?? [],
  );
  expect(reads).toEqual([]);
});
