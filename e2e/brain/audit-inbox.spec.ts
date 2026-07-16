import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Vault Audit inbox smoke (mocked IPC — no Rust core behind it):
 *  - the section mounts on /brain as a sibling of Scheduled briefs, with the
 *    pending count badged on the collapsed header;
 *  - "Audit now" auto-expands the section and surfaces the run summary line;
 *  - groups render in the stable kind order (contradiction → stale →
 *    broken_link → unlinked_mention → orphan), dropping empty kinds;
 *  - Accept is offered ONLY when the backend staged an `acceptAction`
 *    (dismiss-only findings get no Accept button);
 *  - Dismiss is confirm-then-update: the row leaves the inbox only after
 *    `resolve_audit_finding` resolves, and the badge follows;
 *  - an `audit-updated` backend event triggers a silent refetch (no errors).
 */
test("Vault audit: inbox mounts, groups, resolves, and refetches on event", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockTauri(page, {
    list_audit_findings: () => [
      {
        id: "f-1",
        kind: "contradiction",
        sourceKind: "note",
        sourceId: "n-1",
        sourceTitle: "Project Atlas decision",
        targetTitle: "Atlas kickoff meeting",
        evidenceMd: "One note says **ship in Q3**, the other **Q4**.",
        acceptAction: "",
        status: "pending",
        createdAt: 1752600000,
        resolvedAt: null,
      },
      {
        id: "f-2",
        kind: "broken_link",
        sourceKind: "note",
        sourceId: "n-2",
        sourceTitle: "Roadmap 2026",
        targetTitle: "Pricing experiments",
        evidenceMd: "`[[Pricing experiments]]` has no matching note.",
        acceptAction: "Create the missing note “Pricing experiments”",
        status: "pending",
        createdAt: 1752600001,
        resolvedAt: null,
      },
      {
        id: "f-3",
        kind: "orphan",
        sourceKind: "note",
        sourceId: "n-3",
        sourceTitle: "Scratch note",
        targetTitle: null,
        evidenceMd: "No links in, no links out.",
        acceptAction: "",
        status: "pending",
        createdAt: 1752600002,
        resolvedAt: null,
      },
    ],
    run_vault_audit: () => ({
      runId: "run-1",
      startedAt: 1752600100,
      finishedAt: 1752600101,
      findingsNew: 2,
      findingsTotalPending: 3,
      counts: { contradiction: 1, broken_link: 1, orphan: 1 },
    }),
    resolve_audit_finding: (args: { id: string; action: string }) => ({
      id: args.id,
      kind: "contradiction",
      sourceKind: "note",
      sourceId: "n-1",
      sourceTitle: "Project Atlas decision",
      targetTitle: null,
      evidenceMd: "",
      acceptAction: "",
      status: args.action === "accept" ? "accepted" : "dismissed",
      createdAt: 1752600000,
      resolvedAt: 1752600200,
    }),
  });

  await page.goto("/brain");

  const section = page.locator("app-audit");
  await expect(section).toBeVisible();

  // Collapsed header badges the pending count; the inbox body stays hidden.
  await expect(section.locator(".au-count")).toHaveText("3");
  await expect(section.locator(".au-group")).toHaveCount(0);

  // A backend audit-updated ping refetches silently (badge unchanged, no errors).
  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://audit-updated", { findingsTotalPending: 3 });
  });
  await expect(section.locator(".au-count")).toHaveText("3");

  // "Audit now" auto-expands and surfaces the summary line.
  await section.locator(".au-run").click();
  await expect(section.locator(".au-toggle")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(section.locator(".au-summary")).toHaveText(
    /2 new findings · 3 pending/,
  );

  // Groups render in the stable kind order, empty kinds dropped.
  await expect(section.locator(".au-group-title")).toHaveText([
    "Contradictions",
    "Broken links",
    "Orphans",
  ]);

  // Accept only where an acceptAction was staged; dismiss-only rows get none.
  const contradiction = section
    .locator(".au-item")
    .filter({ hasText: "Project Atlas decision" });
  const brokenLink = section
    .locator(".au-item")
    .filter({ hasText: "Roadmap 2026" });
  await expect(contradiction.locator(".btn-primary")).toHaveCount(0);
  await expect(brokenLink.locator(".btn-primary")).toHaveAttribute(
    "title",
    "Create the missing note “Pricing experiments”",
  );

  // Dismiss = confirm-then-update: the row leaves only after the IPC resolves.
  await contradiction.getByRole("button", { name: "Dismiss" }).click();
  await expect(
    section.locator(".au-item").filter({ hasText: "Project Atlas decision" }),
  ).toHaveCount(0);
  await expect(section.locator(".au-count")).toHaveText("2");

  expect(consoleErrors).toEqual([]);
});
