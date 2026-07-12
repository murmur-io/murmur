import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * FE smoke for the "org-editable-and-fresh" work against the mocked Tauri IPC
 * (no Rust core). Covers:
 *   B1 — the org-item viewer Back button navigates to /notes (was a no-op).
 *   F1 — the Library (Meetings) view is RECORDINGS ONLY: even with org statuses +
 *        items mocked, it shows NO "Shared brains" rail and merges NO org items
 *        (org content lives in the Notes view). This inverts the old 0.9.5
 *        org-in-Meetings unification, per the product decision.
 *   F2 — the /org-item viewer resolves the local source and redirects the AUTHOR
 *        (org_resolve_source → { kind:'document' }) to /notes/:id; a null resolve
 *        renders the read-only view (F3) with the metadata strip.
 * NOT proof of end-to-end behavior (no OCK / no server) — a render + routing +
 * no-console-error smoke. Overrides run PAGE-SIDE (self-contained strings).
 */

const ORG_STATUSES = () => [
  {
    orgId: "org-1",
    name: "Acme Inc.",
    role: "owner",
    memberCount: 3,
    consented: true,
    lastSeq: 42,
    itemCount: 2,
    receivedCount: 2,
    pendingShares: 0,
    // Per-instance active/inactive toggle (origin/murmur#273 follow-up) —
    // Library/Notes' org chip rows filter to contextEnabled orgs only, so a
    // fixture missing this field renders an empty chip row (looks like the
    // org vanished, not like a stale fixture — 2026-07-12).
    contextEnabled: true,
  },
];

const ORG_ITEMS = () => [
  {
    itemId: "it-1",
    title: "Acme onboarding brief",
    authorHint: "kasia",
    createdAt: "2026-07-10T09:00:00Z",
    seq: 2,
  },
  {
    itemId: "it-2",
    title: "Pricing rework notes",
    authorHint: "alex",
    createdAt: "2026-07-09T15:30:00Z",
    seq: 1,
  },
];

test.describe("org-editable + library unification (mocked IPC)", () => {
  /**
   * F1 superseded 2026-07-12 by "feat(org): shared meetings in Library, separate
   * from Notes" (#269): Library's "All meetings" list is STILL recordings only
   * (unchanged), but the content pane now ALSO surfaces a "Shared brains" chip
   * row (mirrors Notes') for org's `kind === "meeting"` items specifically —
   * `kind === "document"` items stay Notes-only. These two mocked ORG_ITEMS carry
   * no `kind` at all (the pre-v2 wire format, unclassified) — proves the
   * unclassified-items-stay-hidden guard, not just the "document" exclusion.
   */
  test("F1 — Library's chip row lists the org; unclassified org items never appear in its meeting list", async ({
    page,
  }) => {
    await mockTauri(page, {
      org_refresh: () => null,
      org_list_statuses: ORG_STATUSES,
      list_org_items: ORG_ITEMS,
      list_meeting_org_shares: () => [],
      list_meetings: () => [
        {
          id: "m-1",
          startedAt: "2026-07-11T09:00:00Z",
          endedAt: "2026-07-11T09:30:00Z",
          title: "Weekly sync recording",
          durationS: 1800,
          audioPath: null,
          status: "done",
          folderId: null,
        },
      ],
    });
    await page.goto("/library");

    // The recording renders — this IS the Meetings view, still recordings by default.
    await expect(page.getByText("Weekly sync recording")).toBeVisible({
      timeout: 10_000,
    });

    // The "Shared brains" chip row lists the org.
    const orgChip = page.locator(".org-chip", { hasText: "Acme Inc." });
    await expect(orgChip).toHaveCount(1);
    // NO org rows/cards merged into the default "All meetings" list up front.
    await expect(page.getByText("Acme onboarding brief")).toHaveCount(0);
    await expect(page.getByText("Pricing rework notes")).toHaveCount(0);

    // Selecting the org chip shows ITS scoped list. Both mocked items are
    // unclassified (no `kind` — the pre-v2 wire format) — live-found bug fix,
    // 2026-07-12: these used to be silently EXCLUDED (a passive "couldn't be
    // classified" note was the only hint they existed). Shared content must
    // never just vanish, so they now render, each badged "unclassified"
    // rather than shown as a confirmed meeting.
    await orgChip.click();
    await expect(page.getByText("Acme onboarding brief")).toBeVisible();
    await expect(page.getByText("Pricing rework notes")).toBeVisible();
    await expect(page.locator(".unclassified-hint")).toHaveCount(2);
  });

  test("B1+F3 — read-only viewer (null resolve) renders + Back goes to /notes", async ({
    page,
  }) => {
    await mockTauri(page, {
      // Non-author: no local editable source → the read-only view.
      org_resolve_source: () => null,
      org_get_item: () => ({
        itemId: "it-1",
        authorHint: "kasia",
        title: "Acme onboarding brief",
        createdAt: "2026-07-10T09:00:00Z",
        rev: 3,
        markdown: "# Acme onboarding brief\n\n- Kickoff Monday\n- Owner: Kasia",
      }),
      org_refresh: () => null,
      org_list_statuses: ORG_STATUSES,
      list_org_items: ORG_ITEMS,
    });
    await page.goto("/org-item/it-1");

    // The rich read-only view: title + Org Brain badge + revision + body.
    await expect(page.locator(".oi-title")).toHaveText("Acme onboarding brief", {
      timeout: 10_000,
    });
    await expect(page.getByText("Org Brain")).toBeVisible();
    await expect(page.getByText("revision 3")).toBeVisible();
    await expect(page.getByText("Shared by kasia")).toBeVisible();
    // The org name resolves into the metadata strip (best-effort match).
    await expect(page.locator(".oi-org-name")).toHaveText("Acme Inc.");

    // Back → /notes (B1).
    await page.getByRole("button", { name: /notes/i }).first().click();
    await expect(page).toHaveURL(/\/notes$/, { timeout: 10_000 });
  });

  test("F2 — the AUTHOR is redirected to their editable note", async ({
    page,
  }) => {
    await mockTauri(page, {
      // Author: local source resolves → redirect to the editable /notes/:id.
      org_resolve_source: () => ({ kind: "document", sourceId: "note-42" }),
      // Note-editor load path (best-effort defaults for a smooth render).
      get_note: () => ({
        id: "note-42",
        title: "Acme onboarding brief",
        folderId: "f-notes-root",
        markdown: "# Acme onboarding brief\n\nBody.",
        tags: [],
        properties: {},
        updatedAt: Date.now(),
        createdAt: Date.now(),
        exportedPath: null,
        locked: false,
        shared: true,
      }),
      list_note_folders: () => [],
      list_notes: () => [],
    });
    await page.goto("/org-item/it-1");

    // The viewer redirects (replaceUrl) to the editable note editor.
    await expect(page).toHaveURL(/\/notes\/note-42$/, { timeout: 10_000 });
    await expect(page.locator("app-note-editor")).toBeVisible();
  });
});
