import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Brain v3 PR-4 smoke: the FULL-BRAIN graph mounts over a mocked `get_full_graph`,
 * renders its lens chips + canvas with NO console errors (guards NG0600 /
 * forwardRef / paint regressions), a node-type lens toggle filters the view WITHOUT
 * re-fetching (the lens is a client-side computed), and the "Suggested" toggle
 * DOES re-fetch (it changes `includeSuggested` server-side).
 *
 * Call bookkeeping lives on `window` (the mock overrides run page-side, serialized
 * to strings — no closures over test scope), so the test reads it back afterwards.
 */
test("full-brain graph: mounts, lens filters client-side, Suggested re-fetches, clean console", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockTauri(page, {
    // A tiny typed graph: one node of each kind + edges of a few kinds. The
    // `includeSuggested` flag toggles whether a suggested semantic edge appears
    // — proving the re-fetch semantics. Every edge carries `srcKind`/`dstKind`
    // (PR-9 F4) so the FE endpoint-matching by `(kind, id)` resolves them.
    get_full_graph: (args: {
      opts?: { includeSuggested?: boolean } | null;
    }) => {
      const w = window as unknown as { __fullGraphCalls?: number };
      w.__fullGraphCalls = (w.__fullGraphCalls ?? 0) + 1;
      const suggested = !!args?.opts?.includeSuggested;
      return {
        nodes: [
          { id: "e1", kind: "entity", label: "Alice", date: null, degree: 2 },
          { id: "m1", kind: "meeting", label: "Standup", date: "2026-07-01", degree: 2 },
          { id: "n1", kind: "note", label: "Roadmap", date: "2026-07-02", degree: 1 },
          { id: "d1", kind: "document", label: "Spec.pdf", date: "2026-07-03", degree: 1 },
        ],
        edges: [
          { src: "e1", dst: "m1", srcKind: "entity", dstKind: "meeting", kind: "mention", score: 1, status: "active" },
          { src: "m1", dst: "n1", srcKind: "meeting", dstKind: "note", kind: "companion", score: 1, status: "active" },
          ...(suggested
            ? [{ src: "n1", dst: "d1", srcKind: "note", dstKind: "document", kind: "semantic", score: 0.8, status: "suggested" }]
            : []),
        ],
        hasHidden: false,
        totalVisibleNodes: 4,
        edgesTruncated: false,
      };
    },
  });

  await page.goto("/brain");

  // Open the collapsible "Full brain graph" section.
  await page.getByRole("button", { name: /Full brain graph/i }).click();

  const graph = page.locator("app-full-brain-graph");
  await expect(graph).toBeVisible();
  await expect(graph.locator("canvas.fbg-canvas")).toBeVisible();

  // Lens chips render (one per node kind + one per edge kind + Suggested).
  await expect(graph.getByRole("button", { name: "Meetings" })).toBeVisible();
  await expect(graph.getByRole("button", { name: "Wikilinks" })).toBeVisible();
  await expect(graph.getByRole("button", { name: "Suggested" })).toBeVisible();

  // The caption reflects the drawn counts (4 items, 2 active links).
  await expect(graph.getByText(/4 items · 2 links/)).toBeVisible();

  const callsAfterLoad = await page.evaluate(
    () => (window as unknown as { __fullGraphCalls?: number }).__fullGraphCalls ?? 0,
  );

  // Toggle OFF the Meetings node lens → the view recomputes CLIENT-SIDE (no
  // re-fetch): the meeting node + its edges drop, so the count changes but the
  // fetch count does NOT.
  await graph.getByRole("button", { name: "Meetings" }).click();
  await expect(graph.getByText(/3 items · 0 links/)).toBeVisible();
  const callsAfterLens = await page.evaluate(
    () => (window as unknown as { __fullGraphCalls?: number }).__fullGraphCalls ?? 0,
  );
  expect(callsAfterLens).toBe(callsAfterLoad); // NO re-fetch on a lens toggle

  // Toggle ON Suggested → this DOES re-fetch (includeSuggested changes the payload).
  await graph.getByRole("button", { name: "Suggested" }).click();
  await expect
    .poll(async () =>
      page.evaluate(
        () => (window as unknown as { __fullGraphCalls?: number }).__fullGraphCalls ?? 0,
      ),
    )
    .toBeGreaterThan(callsAfterLens);

  expect(consoleErrors).toEqual([]);
});

/**
 * PR-9 F6 (LOCK-MASKING, the leak class): the graph is a KNOWN leak surface — a
 * sealed meeting's TITLE / a sealed doc's NAME must never surface. The backend gates
 * server-side (a sealed node is simply ABSENT from `get_full_graph`), so the FE's
 * contract is: it renders ONLY what the backend returned, and when `hasHidden` is set
 * it discloses that some items are hidden. This test feeds a gated payload (the sealed
 * meeting/note are absent, `hasHidden: true`) and asserts (a) neither sealed label
 * appears anywhere in the DOM, and (b) the "Some items are hidden" banner shows.
 */
test("full-brain graph: a sealed meeting's title/name never renders; the hidden-items banner shows", async ({
  page,
}) => {
  await mockTauri(page, {
    get_full_graph: () => ({
      // Only the OPEN nodes — the gated backend omits the sealed ones entirely.
      nodes: [
        { id: "m-open", kind: "meeting", label: "Open Standup", date: "2026-07-01", degree: 1 },
        { id: "n-open", kind: "note", label: "Open Roadmap", date: "2026-07-02", degree: 1 },
      ],
      edges: [
        { src: "m-open", dst: "n-open", srcKind: "meeting", dstKind: "note", kind: "companion", score: 1, status: "active" },
      ],
      // A folder is sealed-and-not-unlocked → the FE must disclose the hide.
      hasHidden: true,
      totalVisibleNodes: 2,
      edgesTruncated: false,
    }),
  });

  await page.goto("/brain");
  await page.getByRole("button", { name: /Full brain graph/i }).click();

  const graph = page.locator("app-full-brain-graph");
  await expect(graph).toBeVisible();
  await expect(graph.locator("canvas.fbg-canvas")).toBeVisible();

  // The sealed labels are NOWHERE in the rendered graph (leak class).
  await expect(page.getByText("SECRET Meeting", { exact: false })).toHaveCount(0);
  await expect(page.getByText("secret.pdf", { exact: false })).toHaveCount(0);
  await expect(page.getByText("Locked", { exact: false })).toHaveCount(0);

  // The honest disclosure surfaces.
  await expect(graph.getByText(/Some items are hidden/i)).toBeVisible();
});

/**
 * PR-9 F6 (CAP DISCLOSURE, the silent-trim class): with more visible nodes than the
 * FE draw cap (MAX_NODES = 140), the disclosure banner must name what is ACTUALLY
 * DRAWN, not the backend's post-per-kind-cap `nodes.length`. The toolbar caption
 * ("N items") and the banner ("Drawing N of M items") must AGREE on the drawn count —
 * the F1 fix (before it, the caption said 300 while 140 somas were painted and the
 * banner claimed the backend total). Feeds 300 visible nodes with `totalVisibleNodes`
 * 300; asserts the drawn caption count === the banner's disclosed drawn count === 140.
 */
test("full-brain graph: the draw-cap disclosure names the DRAWN count, and the caption agrees", async ({
  page,
}) => {
  await mockTauri(page, {
    get_full_graph: () => {
      // 300 note nodes (> the 140 draw cap). Give each a distinct degree so the
      // top-140-by-degree selection is deterministic.
      const nodes = Array.from({ length: 300 }, (_v, i) => ({
        id: `n${String(i).padStart(4, "0")}`,
        kind: "note",
        label: `Note ${i}`,
        date: "2026-07-01T00:00:00Z",
        degree: 300 - i,
      }));
      return {
        nodes,
        edges: [],
        hasHidden: false,
        totalVisibleNodes: 300,
        edgesTruncated: false,
      };
    },
  });

  await page.goto("/brain");
  await page.getByRole("button", { name: /Full brain graph/i }).click();

  const graph = page.locator("app-full-brain-graph");
  await expect(graph).toBeVisible();

  // The banner discloses the DRAWN count (140), not the backend total (300).
  await expect(graph.getByText(/Drawing 140 of 300 items/)).toBeVisible();

  // The toolbar caption's item count MUST match what the banner says is drawn (140) —
  // no more claiming more items than the canvas paints (the F1 fix).
  await expect(graph.getByText(/140 items ·/)).toBeVisible();
});
