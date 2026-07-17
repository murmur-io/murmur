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
    // — proving the re-fetch semantics.
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
          { src: "e1", dst: "m1", kind: "mention", score: 1, status: "active" },
          { src: "m1", dst: "n1", kind: "companion", score: 1, status: "active" },
          ...(suggested
            ? [{ src: "n1", dst: "d1", kind: "semantic", score: 0.8, status: "suggested" }]
            : []),
        ],
        hasHidden: false,
        totalVisibleNodes: 4,
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
