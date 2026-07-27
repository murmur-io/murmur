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

/**
 * Phase 3 — weekly schedule chip + per-finding "Explain (AI)" (mocked IPC):
 *  - the passive header chip renders from `get_audit_schedule`
 *    ("Weekly: on · last run yesterday") without expanding the section;
 *  - "Explain (AI)" fetches `explain_audit_finding` and renders the returned
 *    markdown INLINE in the row, with the provider name subtly attached;
 *  - once loaded the button toggles the collapsible block (no re-fetch);
 *  - a rejection (consent-missing / Locked, verbatim) raises a danger toast
 *    and leaves the row unchanged — no explanation block appears.
 */
test("Vault audit: schedule chip renders and Explain (AI) loads inline or toasts", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockTauri(page, {
    get_audit_schedule: () => ({
      enabled: true,
      // Exactly one day back → the chip's relative label is "yesterday".
      lastRunAt: Math.floor(Date.now() / 1000) - 86_400,
      nextDueAt: Math.floor(Date.now() / 1000) + 6 * 86_400,
    }),
    list_audit_findings: () => [
      {
        id: "f-ok",
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
        id: "f-err",
        kind: "orphan",
        sourceKind: "note",
        sourceId: "n-2",
        sourceTitle: "Scratch note",
        targetTitle: null,
        evidenceMd: "No links in, no links out.",
        acceptAction: "",
        status: "pending",
        createdAt: 1752600001,
        resolvedAt: null,
      },
    ],
    explain_audit_finding: (args: { id: string }) => {
      if (args.id === "f-err") {
        throw new Error(
          "AI consent required — enable cloud assistance in Settings",
        );
      }
      return {
        findingId: args.id,
        explanationMd:
          "These two notes disagree because **Q3 slipped to Q4** after the June review.",
        provider: "Claude",
      };
    },
  });

  await page.goto("/brain");
  const section = page.locator("app-audit");
  await expect(section).toBeVisible();

  // The passive schedule chip renders in the (still collapsed) header.
  await expect(section.locator(".au-weekly")).toHaveText(
    /Weekly: on · last run yesterday/,
  );

  // Expand the inbox.
  await section.locator(".au-toggle").click();
  const okRow = section
    .locator(".au-item")
    .filter({ hasText: "Project Atlas decision" });
  const errRow = section
    .locator(".au-item")
    .filter({ hasText: "Scratch note" });

  // Explain success: the markdown renders inline + the provider is credited.
  await okRow.getByRole("button", { name: "Explain (AI)" }).click();
  await expect(okRow.locator(".au-explain strong")).toHaveText(
    "Q3 slipped to Q4",
  );
  await expect(okRow.locator(".au-explain-provider")).toHaveText(/via Claude/);

  // Once loaded the button toggles the collapsible block instead of re-fetching.
  await okRow.getByRole("button", { name: "Hide explanation" }).click();
  await expect(okRow.locator(".au-explain")).toHaveCount(0);
  await okRow.getByRole("button", { name: "Show explanation" }).click();
  await expect(okRow.locator(".au-explain")).toBeVisible();

  // Explain rejection: danger toast, row unchanged.
  //
  // P3: the toast no longer echoes the backend's own sentence. `explain_audit_finding` rejects
  // with an UN-CODED failure here, so `ErrorCopyService.humanize` renders the generic sentence
  // (deny-by-default) rather than a Rust string. What this spec protects is unchanged — that a
  // rejection is SURFACED and leaves the row untouched — not which words appear.
  await errRow.getByRole("button", { name: "Explain (AI)" }).click();
  await expect(page.locator(".toast.is-danger .toast-msg")).toHaveText(
    /Something went wrong\. Please try again\./,
  );
  await expect(errRow.locator(".au-explain")).toHaveCount(0);
  await expect(
    errRow.getByRole("button", { name: "Explain (AI)" }),
  ).toBeEnabled();
  await expect(errRow.getByRole("button", { name: "Dismiss" })).toBeVisible();

  expect(consoleErrors).toEqual([]);
});

/**
 * Phase 3 — the Settings → AI & Models "Weekly vault audit" toggle
 * (confirm-then-update, never optimistic):
 *  - the switch loads DISABLED until `get_audit_schedule` resolves, then
 *    mirrors the backend state;
 *  - flipping it calls `set_audit_schedule { enabled }` and settles on the
 *    RESPONSE;
 *  - a rejected commit raises a danger toast and the switch reverts to the
 *    last confirmed state.
 */
test("Settings: weekly-audit toggle commits via set_audit_schedule and reverts on failure", async ({
  page,
}) => {
  await mockTauri(page, {
    get_audit_schedule: () => ({
      enabled: false,
      lastRunAt: null,
      nextDueAt: null,
    }),
    set_audit_schedule: (args: { enabled: boolean }) => {
      const w = window as unknown as { __setAuditCalls: boolean[] };
      w.__setAuditCalls = [...(w.__setAuditCalls ?? []), args.enabled];
      if (args.enabled === false) {
        // Second flip (on → off) rejects, proving the revert path.
        throw new Error("Locked: unlock the vault to change the schedule");
      }
      return { enabled: args.enabled, lastRunAt: null, nextDueAt: null };
    },
  });

  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();

  const row = page
    .locator("app-on-device-intelligence-block label.toggle-row")
    .filter({ hasText: "Weekly vault audit" });
  const input = row.locator("mur-toggle input");

  // Loaded state mirrors the backend: off, and interactive.
  await expect(input).toBeEnabled();
  await expect(input).not.toBeChecked();

  // Flip ON → set_audit_schedule({ enabled: true }) confirms → stays on.
  await input.click();
  await expect(input).toBeChecked();
  await expect(input).toBeEnabled();

  // Flip OFF → the backend rejects → toast + revert to the confirmed ON.
  //
  // P3: the rejection here is un-coded, so the toast is the generic sentence rather than the raw
  // backend string. The behaviour under test — a rejected commit raises a danger toast and the
  // switch reverts to the last CONFIRMED state — is untouched.
  await input.click();
  await expect(page.locator(".toast.is-danger .toast-msg")).toHaveText(
    /Something went wrong\. Please try again\./,
  );
  await expect(input).toBeChecked();
  await expect(input).toBeEnabled();

  // The command saw exactly the two user flips, in order.
  expect(
    await page.evaluate(
      () => (window as unknown as { __setAuditCalls: boolean[] }).__setAuditCalls,
    ),
  ).toEqual([true, false]);
});
