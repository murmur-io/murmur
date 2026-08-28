import { expect, test, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * `<mur-copy-id>` — the header control that puts an item's stable id on the clipboard so the
 * user can point Claude at THAT meeting/note/board over the local MCP server.
 *
 * The clipboard itself is stubbed rather than driven through a real permission grant: WebKit
 * does not implement Playwright's `clipboard-read`/`clipboard-write` permissions, so a
 * permission-based test would only ever run on one of the two projects. Stubbing
 * `navigator.clipboard.writeText` runs on both AND is the only way to exercise the REFUSAL
 * path, which is the half that matters — an id is not on screen anywhere, so a silently
 * swallowed refusal would leave the user pasting whatever the clipboard held before.
 */
async function stubClipboard(page: Page, mode: "resolve" | "reject"): Promise<void> {
  await page.addInitScript((behaviour: string) => {
    const copied: string[] = [];
    (window as unknown as { __copied: string[] }).__copied = copied;
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: (text: string) => {
          if (behaviour === "reject") {
            return Promise.reject(new Error("NotAllowedError"));
          }
          copied.push(text);
          return Promise.resolve();
        },
      },
    });
  }, mode);
}

const copied = (page: Page) =>
  page.evaluate(() => (window as unknown as { __copied: string[] }).__copied);

test("the meeting header copies the meeting's exact id and confirms it", async ({ page }) => {
  await stubClipboard(page, "resolve");
  await mockTauri(page, {}, { audit_reminder_suggestions: [] });
  await page.goto("/meeting/m-atlas-roadmap");

  const button = page.getByRole("button", { name: "Copy this meeting’s ID" });
  await expect(button).toBeVisible({ timeout: 10_000 });
  await button.click();

  // Verbatim: the string on the clipboard is exactly what `get_meeting`'s `meetingId` takes.
  // A prefix, a label or surrounding punctuation would have to be stripped by hand first.
  expect(await copied(page)).toEqual(["m-atlas-roadmap"]);
  await expect(page.locator(".toast.is-success .toast-msg")).toHaveText(
    "Meeting ID copied",
  );
  await expect(
    page.getByRole("button", { name: "Meeting ID copied" }),
  ).toBeVisible();
});

test("a refused clipboard write says so instead of flashing a false confirmation", async ({
  page,
}) => {
  await stubClipboard(page, "reject");
  await mockTauri(page, {}, { audit_reminder_suggestions: [] });
  await page.goto("/meeting/m-atlas-roadmap");

  const button = page.getByRole("button", { name: "Copy this meeting’s ID" });
  await expect(button).toBeVisible({ timeout: 10_000 });
  await button.click();

  await expect(page.locator(".toast.is-danger .toast-msg")).toHaveText(
    "Couldn’t copy the meeting ID — your Mac refused clipboard access.",
  );
  // The control must NOT claim success: no tick, and no success toast alongside the failure.
  await expect(page.getByRole("button", { name: "Meeting ID copied" })).toHaveCount(0);
  await expect(page.locator(".toast.is-success")).toHaveCount(0);
});

test("the note header copies the note's id", async ({ page }) => {
  await stubClipboard(page, "resolve");
  await mockTauri(page, {});
  await page.goto("/notes/n1");

  const button = page.getByRole("button", { name: "Copy this note’s ID" });
  await expect(button).toBeVisible({ timeout: 10_000 });
  await button.click();

  expect(await copied(page)).toEqual(["n1"]);
  await expect(page.locator(".toast.is-success .toast-msg")).toHaveText(
    "Note ID copied",
  );
});

const TASK_ORG_ID = "11111111-1111-4111-8111-111111111111";
const TASK_DOC_ID = "22222222-2222-4222-8222-222222222222";
const TASK_ID = `${TASK_ORG_ID}:${TASK_DOC_ID}`;

/**
 * A task id is copyable too, but the tooltip deliberately promises nothing about Claude here:
 * no MCP tool resolves a task id yet, so this control exists to let the user refer to the task
 * by hand. If a `get_task` tool ever lands, the copy is already in the right place.
 */
test("the task header copies the task's composite id", async ({ page }) => {
  await stubClipboard(page, "resolve");
  await mockTauri(
    page,
    {
      org_refresh: () => null,
      task_list_assignees: () => [],
      list_note_attachments: () => [],
      list_tasks: () => [
        {
          id: "11111111-1111-4111-8111-111111111111:22222222-2222-4222-8222-222222222222",
          orgId: "11111111-1111-4111-8111-111111111111",
          docId: "22222222-2222-4222-8222-222222222222",
          itemId: "33333333-3333-4333-8333-333333333333",
          sourceDocumentId: null,
          version: 1,
          title: "Finish onboarding",
          description: "Body",
          status: "inProgress",
          dueAt: null,
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
        },
      ],
    },
    {
      org_list_statuses: [
        {
          orgId: TASK_ORG_ID,
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
      ],
    },
  );
  await page.goto(`/tasks/${TASK_ID}`);

  const button = page.getByRole("button", { name: "Copy this task\u2019s ID" });
  await expect(button).toBeVisible({ timeout: 10_000 });
  await button.click();

  expect(await copied(page)).toEqual([TASK_ID]);
  await expect(page.locator(".toast.is-success .toast-msg")).toHaveText(
    "Task ID copied",
  );
});

test("the board header copies the board's id from the title line", async ({ page }) => {
  await stubClipboard(page, "resolve");
  await mockTauri(
    page,
    {},
    {
      list_dashboards: [
        { id: "b-copy", title: "Atlas GA", emoji: "\ud83d\ude80", createdAt: 1, updatedAt: 2, tileKinds: [] },
      ],
      get_dashboard: {
        id: "b-copy",
        title: "Atlas GA",
        emoji: "\ud83d\ude80",
        createdAt: 1,
        updatedAt: 2,
        tileKinds: [],
        tiles: [],
      },
      get_dashboard_sources: [],
    },
  );
  await page.goto("/dashboards/b-copy");

  const button = page.getByRole("button", { name: "Copy this board\u2019s ID" });
  await expect(button).toBeVisible({ timeout: 10_000 });
  await button.click();

  expect(await copied(page)).toEqual(["b-copy"]);
  await expect(page.locator(".toast.is-success .toast-msg")).toHaveText(
    "Board ID copied",
  );
});
