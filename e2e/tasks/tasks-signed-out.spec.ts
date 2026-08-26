import { expect, test, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * 2.0 release blocker: Tasks is ORG-ONLY (`commands/tasks.rs::list_tasks` opens with
 * `session_server_user_id`), but the DEFAULT Murmur user is local-first with no account — the
 * product's headline promise. Clicking the Tasks nav item therefore rendered the raw `AppError`
 * `Display` string in a red banner, on top of an empty state that told the user to "create shared
 * work" and a New task button that also failed.
 *
 * The oracle has two legs and BOTH must hold:
 *   RED  — a signed-out device gets a purposeful invitation and ZERO developer prose.
 *   CONTROL — a signed-IN device still renders the real task list, so the gate cannot go vacuous
 *             by simply swallowing Tasks for everybody.
 */

const ORG_ID = "11111111-1111-4111-8111-111111111111";
const DOC_ID = "22222222-2222-4222-8222-222222222222";
const TASK_ID = `${ORG_ID}:${DOC_ID}`;

/** The exact wire string `share::require_login` produces once it carries its `[code]`. */
const SIGNED_OUT_WIRE =
  "provider unavailable: [sharing-account-required] not signed in to the sharing account";

/** Vocabulary that must never reach a person, per `src-tauri/src/error.rs`. */
const DEVELOPER_PROSE = [
  "not signed in to the sharing account",
  "provider unavailable",
  "sharing-account-required",
  "AppError",
];

const ORGS = [
  {
    orgId: ORG_ID,
    name: "Acme",
    role: "member",
    memberCount: 2,
    consented: true,
    lastSeq: 7,
    itemCount: 1,
    receivedCount: 1,
    pendingShares: 0,
    contextEnabled: true,
  },
];

const TASK = {
  id: TASK_ID,
  orgId: ORG_ID,
  docId: DOC_ID,
  itemId: "33333333-3333-4333-8333-333333333333",
  sourceDocumentId: null,
  version: 1,
  title: "Finish onboarding",
  description: "Ship the shared task view.",
  status: "inProgress",
  dueAt: "2026-08-28T12:00:00Z",
  assigneeUserId: null,
  createdAt: "2026-08-20T09:00:00Z",
  subtasks: [],
  orgRefs: [],
  images: [],
  access: "view",
  canEdit: false,
  canManage: false,
  localRefs: [],
  updatedAt: "2026-08-21T09:00:00Z",
};

function watchRuntimeErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  return errors;
}

test("a signed-out device gets a purposeful Tasks invitation, never the raw refusal", async ({
  page,
}) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await mockTauri(
    page,
    {
      org_refresh: () => null,
      org_list_statuses: () => [],
      // Exactly what the Rust seam returns for a device that has never signed in.
      list_tasks: () => {
        throw new Error(
          "provider unavailable: [sharing-account-required] not signed in to the sharing account",
        );
      },
      list_dashboards: () => [],
    },
    {},
  );

  await page.goto("/tasks");

  await expect(
    page.getByRole("heading", { name: "Tasks live in a Shared Brain", level: 1 }),
  ).toBeVisible();
  await expect(
    page.getByText("Tasks are shared work inside an organization", { exact: false }),
  ).toBeVisible();

  // The two doors the account banner already offers — same vocabulary, one flow.
  await expect(page.getByRole("button", { name: "Sign in" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Create account" })).toBeVisible();

  // No error banner at all: this is an expected state, not a failure.
  await expect(page.getByRole("alert")).toHaveCount(0);
  // And the misleading empty state is gone with it.
  await expect(page.getByText("No tasks here.")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "New task" })).toHaveCount(0);

  const rendered = (await page.locator("body").innerText()).toLowerCase();
  for (const phrase of DEVELOPER_PROSE) {
    expect(rendered).not.toContain(phrase.toLowerCase());
  }

  // The CTA is wired to the ONE existing account surface, not decorative.
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { name: "Sign in", level: 2 })).toBeVisible();
  await expect(page.getByPlaceholder("you@example.com")).toBeVisible();

  expect(runtimeErrors).toEqual([]);
  // The wire string the mock threw is the one the Rust seam produces — pinned here so a rename on
  // either side fails this test instead of silently reverting the surface to prose.
  expect(SIGNED_OUT_WIRE).toContain("[sharing-account-required]");
});

test("CONTROL: a signed-in device still renders the real task list", async ({
  page,
}) => {
  const runtimeErrors = watchRuntimeErrors(page);
  await mockTauri(
    page,
    {
      org_refresh: () => null,
      task_list_assignees: () => [],
      list_note_attachments: () => [],
    },
    {
      org_list_statuses: ORGS,
      list_tasks: [TASK],
      list_dashboards: [],
    },
  );

  await page.goto("/tasks");

  await expect(page.getByRole("heading", { name: "Tasks", level: 1 })).toBeVisible();
  await expect(page.getByText("Finish onboarding")).toBeVisible();
  await expect(page.getByRole("button", { name: "New task" }).first()).toBeVisible();
  // The gate must NOT appear for someone who has an account.
  await expect(
    page.getByRole("heading", { name: "Tasks live in a Shared Brain" }),
  ).toHaveCount(0);

  expect(runtimeErrors).toEqual([]);
});

test("an UN-CODED task failure degrades to fixed copy instead of leaking Rust prose", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      org_refresh: () => null,
      org_list_statuses: () => [],
      list_tasks: () => {
        throw new Error("storage error: account-session mutex poisoned");
      },
      list_dashboards: () => [],
    },
    {},
  );

  await page.goto("/tasks");

  const alert = page.getByRole("alert");
  await expect(alert).toContainText("Couldn’t load shared tasks. Please try again.");
  const rendered = (await page.locator("body").innerText()).toLowerCase();
  expect(rendered).not.toContain("mutex poisoned");
  expect(rendered).not.toContain("storage error");
  // CONTROL for this leg: an un-coded failure is an ERROR, so the invitation must NOT hijack it.
  await expect(
    page.getByRole("heading", { name: "Tasks live in a Shared Brain" }),
  ).toHaveCount(0);
});
