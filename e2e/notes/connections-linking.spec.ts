import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * PR-1 — user-initiated linking UI in the shared `app-connections` panel (mounted
 * in the note editor at /notes/n1). Verifies, against a mocked Tauri IPC:
 *  - `list_links` renders a mix of chips; ONLY the `manual:true` chip shows a `×`.
 *  - clicking that `×` invokes `unlink_items` with the anchor→neighbour args and
 *    then re-fetches `list_links` (the chip drops out).
 *  - the `+ Link` chooser opens the OPAQUE single-pick popover (`app-link-picker`),
 *    and picking a candidate invokes `link_items(anchorKind, anchorId, kind, id)`
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
          otherKind: "note",
          otherId: "n-picked",
          otherTitle: "Picked Note",
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

    // The single-pick chooser's candidate feed: a note + a meeting + an org row
    // (org must be filtered out of the chooser as a non-linkable endpoint).
    list_link_candidates: () => [
      { kind: "note", id: "n-picked", title: "Picked Note", snippet: "" },
      { kind: "meeting", id: "m-cand", title: "Some meeting", snippet: "" },
      { kind: "org", id: "org-1", title: "Org item", snippet: "" },
    ],

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

  // Three chips render; the manual one has a × remove button, the others do not.
  await expect(panel.getByText("Manual Meeting Link")).toBeVisible();
  await expect(panel.getByText("Weekly plan")).toBeVisible();
  await expect(panel.getByText("A suggested note")).toBeVisible();

  // Exactly ONE remove (×) button — on the manual chip only.
  const removeButtons = panel.locator("button.cx-remove");
  await expect(removeButtons).toHaveCount(1);
  await expect(
    panel.getByRole("button", { name: "Remove link to Manual Meeting Link" }),
  ).toBeVisible();

  // Click the × → unlink_items(anchorKind=note, anchorId=n1, otherKind=meeting,
  // otherId=m-manual) is invoked, then the re-fetch drops the manual chip.
  await removeButtons.first().click();
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

  // Open the + Link chooser → the query input + the opaque picker popover appear.
  await panel.getByRole("button", { name: "Link" }).click();
  await expect(panel.getByPlaceholder(/Link a meeting, note/)).toBeVisible();
  // The link-picker popover box is TELEPORTED to <body> (appTeleportToBody) —
  // locate it by class, not by the `app-link-picker` host.
  const picker = page.locator(".link-pop");
  await expect(picker).toBeVisible();

  // The picker offers the candidates (org row is not a valid endpoint; picking a
  // note candidate invokes link_items(anchor=note/n1 → note/n-picked)).
  const pickedRow = picker.getByRole("option", { name: /Picked Note/ });
  await expect(pickedRow).toBeVisible();
  // mousedown is prevented on the popover (no focus steal), then click emits picked.
  await pickedRow.dispatchEvent("click");

  // The new manual chip appears from the re-fetch; link_items was called correctly.
  await expect(panel.getByText("Picked Note")).toBeVisible();
  const afterLink = await page.evaluate(
    () => (window as unknown as { __linkCalls?: unknown[] }).__linkCalls || [],
  );
  expect(afterLink).toContainEqual({
    cmd: "link_items",
    args: { srcKind: "note", srcId: "n1", dstKind: "note", dstId: "n-picked" },
  });

  expect(consoleErrors).toEqual([]);
});
