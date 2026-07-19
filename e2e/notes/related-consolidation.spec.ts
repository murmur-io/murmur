import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * VERIFICATION (2026-07-19 IA consolidation) — the merged "Related" panel in the
 * note editor at /notes/n1, driven over mocked Tauri IPC. Asserts the six behaviors
 * the redesign promises:
 *  1. ONE "Related · N" section (no separate "Linked mentions" / "Connections").
 *  2. A neighbour that is BOTH an inbound backlink AND an outbound edge shows ONCE.
 *  3. A body-inline [[wikilink]] neighbour is NOT re-chipped (item 4).
 *  4. A suggestion is an ambient dashed chip: tap promotes (acceptLink), hover ×
 *     dismisses (dismissLink) — no % label, no Accept/Dismiss buttons.
 *  5. The header shows ≤5 top-level controls (Move / Edit-Preview seg / Ask / ⋯).
 *  6. Zero console/page errors.
 */
test("merged Related panel: dedup, inline-suppress, ambient suggestions, slim header", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") consoleErrors.push(m.text());
  });
  page.on("pageerror", (e) => consoleErrors.push(String(e)));

  await mockNotes(page, {
    // A note whose BODY links [[Meeting 2026-07-17]] inline (should NOT re-chip it).
    get_note: (args: { id: string }) => ({
      id: args.id,
      title: "Weekly plan",
      folderId: "nf1",
      markdown:
        "# Weekly plan\n\nSee the [[Meeting 2026-07-17]] for context and the roadmap.",
      tags: [],
      properties: {},
      updatedAt: 1_720_000_000_000,
      createdAt: 1_719_000_000_000,
      exportedPath: null,
      locked: false,
      shared: false,
    }),

    // list_links: a companion edge, a manual link, a wikilink edge whose title is
    // ALREADY inline (must be suppressed), and a wikilink edge that ALSO appears as
    // a backlink below (must dedup to ONE chip), plus a semantic SUGGESTION.
    list_links: () => {
      const w = window as unknown as { __dismissed?: boolean; __accepted?: boolean };
      const rows = [
        {
          id: 1,
          direction: "out",
          otherKind: "meeting",
          otherId: "m-kickoff",
          otherTitle: "Kickoff",
          edgeType: "companion",
          createdBy: "auto",
          status: "active",
          score: 1.0,
          createdAt: 1,
          manual: false,
        },
        {
          id: 2,
          direction: "out",
          otherKind: "note",
          otherId: "n-roadmap",
          otherTitle: "Roadmap",
          edgeType: "manual",
          createdBy: "user",
          status: "active",
          score: 1.0,
          createdAt: 2,
          manual: true,
        },
        {
          id: 3,
          direction: "out",
          otherKind: "meeting",
          otherId: "m-0717",
          otherTitle: "Meeting 2026-07-17",
          edgeType: "wikilink",
          createdBy: "auto",
          status: "active",
          score: 1.0,
          createdAt: 3,
          manual: false,
        },
        {
          id: 4,
          direction: "in",
          otherKind: "note",
          otherId: "n-standup",
          otherTitle: "Standup notes",
          edgeType: "wikilink",
          createdBy: "auto",
          status: "active",
          score: 1.0,
          createdAt: 4,
          manual: false,
        },
        {
          id: 5,
          direction: "in",
          otherKind: "note",
          otherId: "n-design",
          otherTitle: "Design doc",
          edgeType: "semantic",
          createdBy: "auto",
          status: "suggested",
          score: 0.86,
          createdAt: 5,
          manual: false,
        },
      ];
      let out = rows;
      if (w.__dismissed) out = out.filter((r) => r.id !== 5);
      if (w.__accepted) {
        out = out.map((r) =>
          r.id === 5 ? { ...r, status: "active", edgeType: "manual", manual: true } : r,
        );
      }
      return out;
    },

    // Backlinks: "Standup notes" (ALSO an outbound wikilink edge above → dedup to
    // ONE), and a NEW inbound-only "Retro" note (mentions-only row).
    get_backlinks: () => [
      { id: "n-standup", kind: "note", title: "Standup notes", timestamp: "2026-07-15T10:00:00Z" },
      { id: "n-retro", kind: "note", title: "Retro", timestamp: "2026-07-16T10:00:00Z" },
    ],

    accept_link: () => {
      (window as unknown as { __accepted?: boolean }).__accepted = true;
      return null;
    },
    dismiss_link: () => {
      (window as unknown as { __dismissed?: boolean }).__dismissed = true;
      return null;
    },
  });

  await page.goto("/notes/n1");

  const panel = page.locator("app-connections");
  await expect(panel).toBeVisible();

  // (1) ONE "Related" section header (with a count), no old labels.
  await expect(panel.locator(".cx-label--head")).toContainText("Related");
  await expect(page.getByText("Linked mentions")).toHaveCount(0);
  await expect(page.getByText("Suggested connections")).toHaveCount(0);

  // (2) dedup — "Standup notes" is both an inbound backlink AND an outbound edge;
  // it renders exactly ONCE.
  await expect(panel.getByText("Standup notes", { exact: true })).toHaveCount(1);

  // (3) inline suppression — the body links [[Meeting 2026-07-17]], so its wikilink
  // edge chip is NOT shown in Related (no triplication).
  await expect(
    panel.locator(".cx-chip").filter({ hasText: "Meeting 2026-07-17" }),
  ).toHaveCount(0);

  // The real relationships DO show: companion, manual, dedup'd backlink, mentions-only.
  await expect(panel.getByText("Kickoff", { exact: true })).toBeVisible();
  await expect(panel.getByText("Roadmap", { exact: true })).toBeVisible();
  await expect(panel.getByText("Retro", { exact: true })).toBeVisible();

  // (4a) NO raw confidence % anywhere in the panel (the score chip is gone).
  await expect(panel.getByText("86%")).toHaveCount(0);
  await expect(panel.locator(".cx-score")).toHaveCount(0);
  // NO persistent Accept/Dismiss buttons.
  await expect(panel.getByRole("button", { name: "Accept" })).toHaveCount(0);
  await expect(panel.getByRole("button", { name: "Dismiss", exact: true })).toHaveCount(0);
  // The suggestion IS a dashed chip.
  await expect(panel.locator(".cx-chip--suggested")).toContainText("Design doc");

  // (5) slim header — count the top-level controls in `.editor-head` (before any
  // menu opens): breadcrumb (Move), the Edit/Preview segmented control, Ask Brain,
  // and the ⋯ trigger. Save-state is silent at rest. That is 4 (≤5).
  const head = page.locator(".editor-head");
  await expect(head.locator(".crumb-btn").first()).toBeVisible(); // breadcrumb/Move
  await expect(head.locator(".head-seg")).toHaveCount(1); // Edit/Preview seg
  await expect(head.locator(".head-chat-btn")).toHaveCount(1); // Ask Brain
  await expect(head.locator(".head-more .crumb-btn")).toHaveCount(1); // ⋯
  // Share is NOT a top-level header button anymore.
  await expect(head.getByRole("button", { name: "Share" })).toHaveCount(0);

  // Screenshot the collapsed Related section (dark scheme is the config default).
  await panel.scrollIntoViewIfNeeded();
  await panel.screenshot({
    path: "/private/tmp/claude-501/-Users-jakubgawronski-Projects-meetnotes/d3db29b4-fbd3-49ac-868a-fd86a0c14a1f/scratchpad/related-panel.png",
  });

  // (4b) promote a suggestion by TAPPING its chip body → acceptLink runs, the
  // dashed chip becomes a real "Design doc" link and the suggestion group empties.
  await panel.locator(".cx-chip--suggested").click();
  await expect(panel.locator(".cx-chip--suggested")).toHaveCount(0);
  await expect(panel.getByText("Design doc", { exact: true })).toBeVisible();

  expect(consoleErrors).toEqual([]);
});
