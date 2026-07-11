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
  test("F1 — Library (Meetings) is recordings ONLY: no Shared brains, no org items", async ({
    page,
  }) => {
    await mockTauri(page, {
      org_refresh: () => null,
      // Org statuses + items are mocked but MUST be ignored by the Meetings view.
      org_list_statuses: ORG_STATUSES,
      list_org_items: ORG_ITEMS,
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

    // The recording renders — this IS the Meetings view.
    await expect(page.getByText("Weekly sync recording")).toBeVisible({
      timeout: 10_000,
    });

    // NO "Shared brains" rail section.
    await expect(page.getByText("Shared brains")).toHaveCount(0);
    // NO org rows, NO merged org cards, NO org-item links — org content is
    // Notes-only now.
    await expect(page.locator(".org-row")).toHaveCount(0);
    await expect(page.locator("a[href^='/org-item/']")).toHaveCount(0);
    await expect(page.getByText("Acme onboarding brief")).toHaveCount(0);
    await expect(page.getByText("Pricing rework notes")).toHaveCount(0);
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
