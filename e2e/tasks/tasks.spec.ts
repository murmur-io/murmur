import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

const ORG_ID = "11111111-1111-4111-8111-111111111111";
const DOC_ID = "22222222-2222-4222-8222-222222222222";
const TASK_ID = `${ORG_ID}:${DOC_ID}`;

const TASK = {
  id: TASK_ID,
  orgId: ORG_ID,
  docId: DOC_ID,
  itemId: "33333333-3333-4333-8333-333333333333",
  sourceDocumentId: null,
  version: 1,
  title: "Finish onboarding",
  description: "Ship the shared task view without leaking it into Ask.",
  status: "inProgress",
  dueAt: "2026-08-28T12:00:00Z",
  assigneeUserId: "44444444-4444-4444-8444-444444444444",
  createdAt: "2026-08-20T09:00:00Z",
  subtasks: [{ id: "sub-1", title: "Verify permissions", done: false }],
  orgRefs: [],
  images: [],
  access: "view",
  canEdit: false,
  canManage: false,
  localRefs: [{ kind: "dashboard", refId: "board-1" }],
  updatedAt: "2026-08-21T09:00:00Z",
};

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

const BOARD = {
  id: "board-1",
  title: "Launch board",
  emoji: "🚀",
  tint: "indigo",
  pinned: true,
  position: 0,
  createdAt: "2026-08-20T09:00:00Z",
  updatedAt: "2026-08-21T09:00:00Z",
  tileCount: 0,
  tileKinds: [],
  tiles: [],
  work: [],
};

test("Task View keeps shared fields view-only while local refs remain device-editable", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      org_refresh: () => null,
      task_list_assignees: () => [
        { userId: "44444444-4444-4444-8444-444444444444", label: "Kasia" },
      ],
      list_note_attachments: () => [],
      set_task_local_refs: (args: {
        refs: Array<{ kind: string; refId: string }>;
      }) => {
        const target = window as unknown as {
          __taskLocalRefs?: Array<{ kind: string; refId: string }>;
        };
        target.__taskLocalRefs = args.refs;
        return args.refs;
      },
      list_tasks: () => {
        const target = window as unknown as {
          __taskLocalRefs?: Array<{ kind: string; refId: string }>;
        };
        return [
          {
            id: "11111111-1111-4111-8111-111111111111:22222222-2222-4222-8222-222222222222",
            orgId: "11111111-1111-4111-8111-111111111111",
            docId: "22222222-2222-4222-8222-222222222222",
            itemId: "33333333-3333-4333-8333-333333333333",
            sourceDocumentId: null,
            version: 1,
            title: "Finish onboarding",
            description: "Ship the shared task view without leaking it into Ask.",
            status: "inProgress",
            dueAt: "2026-08-28T12:00:00Z",
            assigneeUserId: "44444444-4444-4444-8444-444444444444",
            createdAt: "2026-08-20T09:00:00Z",
            subtasks: [{ id: "sub-1", title: "Verify permissions", done: false }],
            orgRefs: [],
            images: [],
            access: "view",
            canEdit: false,
            canManage: false,
            localRefs:
              target.__taskLocalRefs ?? [{ kind: "dashboard", refId: "board-1" }],
            updatedAt: "2026-08-21T09:00:00Z",
          },
        ];
      },
    },
    {
      org_list_statuses: ORGS,
      list_dashboards: [BOARD],
    },
  );

  await page.goto(`/tasks/${TASK_ID}`);
  await expect(page.getByRole("heading", { name: "Tasks", level: 1 })).toBeVisible();
  await expect(page.getByText("View only", { exact: true })).toBeVisible();
  await expect(page.getByLabel("Task title")).toHaveValue("Finish onboarding");
  await expect(page.getByLabel("Task title")).toBeDisabled();
  await expect(page.getByRole("button", { name: "Delete" })).toHaveCount(0);
  await expect(page.getByLabel("Complete Verify permissions")).toBeDisabled();

  await expect(page.getByText("dashboard · board-1")).toBeVisible();
  await page.getByLabel("Remove local link").click();
  await expect(page.getByText("dashboard · board-1")).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as {
            __taskLocalRefs?: Array<{ kind: string; refId: string }>;
          }).__taskLocalRefs ?? null,
      ),
    )
    .toEqual([]);
});

test("Task feed invalidation scrubs stale plaintext before the replacement read resolves", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      org_refresh: () => null,
      list_tasks: () => {
        const target = window as unknown as {
          __taskReloadPending?: boolean;
          __resolveTaskReload?: () => void;
        };
        const fresh = {
          id: "11111111-1111-4111-8111-111111111111:22222222-2222-4222-8222-222222222222",
          orgId: "11111111-1111-4111-8111-111111111111",
          docId: "22222222-2222-4222-8222-222222222222",
          itemId: "item-fresh",
          sourceDocumentId: null,
          version: 1,
          title: "Fresh authoritative task",
          description: "New body",
          status: "todo",
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
          updatedAt: "2026-08-21T10:00:00Z",
        };
        if (target.__taskReloadPending) {
          return new Promise((resolve) => {
            target.__resolveTaskReload = () => resolve([fresh]);
          });
        }
        return [
          {
            ...fresh,
            itemId: "item-stale",
            title: "Stale secret task",
            description: "Stale body",
            updatedAt: "2026-08-21T09:00:00Z",
          },
        ];
      },
      list_dashboards: () => [],
      list_note_attachments: () => [],
      task_list_assignees: () => [],
    },
    { org_list_statuses: ORGS },
  );

  await page.goto("/tasks");
  await expect(page.getByText("Stale secret task", { exact: true })).toBeVisible();
  await page.evaluate(() => {
    const target = window as unknown as {
      __taskReloadPending?: boolean;
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__taskReloadPending = true;
    target.__demoEmit("murmur://org-feed-updated", { orgsChanged: 1 });
  });

  await expect(page.getByText("Stale secret task", { exact: true })).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (window as unknown as { __resolveTaskReload?: unknown })
            .__resolveTaskReload,
      ),
    )
    .toBe("function");
  await page.evaluate(() => {
    (window as unknown as { __resolveTaskReload: () => void }).__resolveTaskReload();
  });
  await expect(page.getByText("Fresh authoritative task", { exact: true })).toBeVisible();
});

test("Task feed invalidation cannot restore a stale assignee response", async ({ page }) => {
  await mockTauri(
    page,
    {
      org_refresh: () => null,
      list_note_attachments: () => [],
      task_list_assignees: () => {
        const target = window as unknown as {
          __taskAssigneeCalls?: number;
          __resolveStaleTaskAssignees?: () => void;
        };
        target.__taskAssigneeCalls = (target.__taskAssigneeCalls ?? 0) + 1;
        if (target.__taskAssigneeCalls === 1) {
          return new Promise((resolve) => {
            target.__resolveStaleTaskAssignees = () =>
              resolve([
                {
                  userId: "55555555-5555-4555-8555-555555555555",
                  label: "Stale former member",
                },
              ]);
          });
        }
        return [];
      },
    },
    {
      org_list_statuses: ORGS,
      list_tasks: [{ ...TASK, assigneeUserId: null, canEdit: true }],
      list_dashboards: [],
    },
  );

  await page.goto(`/tasks/${TASK_ID}`);
  await expect(page.getByLabel("Task title")).toHaveValue("Finish onboarding");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (window as unknown as { __resolveStaleTaskAssignees?: unknown })
            .__resolveStaleTaskAssignees,
      ),
    )
    .toBe("function");

  await page.evaluate(() => {
    const target = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__demoEmit("murmur://org-feed-updated", { orgsChanged: 1 });
  });
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __taskAssigneeCalls?: number })
            .__taskAssigneeCalls ?? 0,
      ),
    )
    .toBeGreaterThanOrEqual(2);

  await page.evaluate(() => {
    (window as unknown as { __resolveStaleTaskAssignees: () => void })
      .__resolveStaleTaskAssignees();
  });
  await expect(
    page.getByRole("option", { name: "Stale former member", exact: true }),
  ).toHaveCount(0);
});

test("Task feed invalidation scrubs an unsaved detail draft and only restores the authoritative row", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      org_refresh: () => null,
      list_tasks: () => {
        const target = window as unknown as {
          __dirtyTaskReload?: boolean;
          __resolveDirtyTaskReload?: () => void;
        };
        const row = {
          id: "11111111-1111-4111-8111-111111111111:22222222-2222-4222-8222-222222222222",
          orgId: "11111111-1111-4111-8111-111111111111",
          docId: "22222222-2222-4222-8222-222222222222",
          sourceDocumentId: null,
          version: 1,
          status: "inProgress",
          dueAt: "2026-08-28T12:00:00Z",
          assigneeUserId: null,
          createdAt: "2026-08-20T09:00:00Z",
          subtasks: [],
          orgRefs: [],
          images: [],
          localRefs: [],
          access: "edit",
          canEdit: true,
          canManage: false,
          title: target.__dirtyTaskReload ? "Authoritative replacement" : "Original task",
          description: target.__dirtyTaskReload ? "Replacement body" : "Original body",
          itemId: target.__dirtyTaskReload ? "item-replacement" : "item-original",
          updatedAt: target.__dirtyTaskReload
            ? "2026-08-21T11:00:00Z"
            : "2026-08-21T09:00:00Z",
        };
        if (target.__dirtyTaskReload) {
          return new Promise((resolve) => {
            target.__resolveDirtyTaskReload = () => resolve([row]);
          });
        }
        return [row];
      },
      list_dashboards: () => [],
      list_note_attachments: () => [],
      task_list_assignees: () => [],
    },
    { org_list_statuses: ORGS },
  );

  await page.goto(`/tasks/${TASK_ID}`);
  await expect(page.getByLabel("Task title")).toHaveValue("Original task");
  await page.getByLabel("Task title").fill("Unsaved private draft");
  await page.getByLabel("Task description").fill("Unsaved private body");

  await page.evaluate(() => {
    const target = window as unknown as {
      __dirtyTaskReload?: boolean;
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__dirtyTaskReload = true;
    target.__demoEmit("murmur://org-feed-updated", { orgsChanged: 1 });
  });

  await expect(page.getByLabel("Task title")).toHaveCount(0);
  await expect(page.getByLabel("Task description")).toHaveCount(0);
  await expect(page.getByText("Unsaved private draft", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Unsaved private body", { exact: true })).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (window as unknown as { __resolveDirtyTaskReload?: unknown })
            .__resolveDirtyTaskReload,
      ),
    )
    .toBe("function");

  await page.evaluate(() => {
    (window as unknown as { __resolveDirtyTaskReload: () => void }).__resolveDirtyTaskReload();
  });
  await expect(page.getByLabel("Task title")).toHaveValue("Authoritative replacement");
  await expect(page.getByLabel("Task description")).toHaveValue("Replacement body");
  await expect(page.getByText("Unsaved private draft", { exact: true })).toHaveCount(0);
});

test("Dashboard Work is device-private and opens the shared task", async ({ page }) => {
  await mockTauri(
    page,
    { org_refresh: () => null },
    {
      org_list_statuses: ORGS,
      list_tasks: [TASK],
      list_dashboards: [BOARD],
      get_dashboard: BOARD,
      get_dashboard_sources: [],
    },
  );

  await page.goto("/dashboards/board-1");
  // `exact`, because the sidebar's own `<section aria-label="Workspaces">`
  // substring-matches "Work" and Playwright's accessible-name matching is
  // substring by default.
  const work = page.getByRole("region", { name: "Work", exact: true });
  await expect(work).toBeVisible();
  await expect(work.getByText("Finish onboarding")).toBeVisible();
  await work.getByRole("button", { name: /Finish onboarding/ }).click();
  await expect(page).toHaveURL(new RegExp(`/tasks/${ORG_ID}:${DOC_ID}$`));
});

test("mobile task routes switch cleanly between list, detail, and create", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockTauri(
    page,
    {
      org_refresh: () => null,
      list_note_attachments: () => [],
      task_list_assignees: () => [],
    },
    {
      org_list_statuses: ORGS,
      list_tasks: [TASK],
      list_dashboards: [],
    },
  );

  await page.goto(`/tasks/${TASK_ID}`);
  await expect(page.getByTestId("task-editor")).toBeVisible();
  await expect(page.getByRole("button", { name: "← Tasks" })).toBeVisible();
  await page.getByRole("button", { name: "← Tasks" }).click();
  await expect(page).toHaveURL(/\/tasks$/);
  await expect(page.getByRole("button", { name: /Finish onboarding/ })).toBeVisible();

  await page.getByRole("button", { name: "New task" }).first().click();
  await expect(page).toHaveURL(/\/tasks\/new$/);
  await expect(page.getByLabel("Task title")).toBeVisible();
  await expect(
    page.getByRole("combobox", { name: "Organization", exact: true }),
  ).toHaveValue(ORG_ID);
});

test("creating a task sends the canonical org draft and opens its stable detail route", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      org_refresh: () => null,
      list_note_attachments: () => [],
      task_list_assignees: () => [],
      list_tasks: () => {
        const target = window as unknown as { __createdTask?: unknown };
        return target.__createdTask ? [target.__createdTask] : [];
      },
      create_task: (args: {
        draft: {
          orgId: string;
          title: string;
          description: string;
          status: string;
          dueAt: string | null;
          assigneeUserId: string | null;
          subtasks: unknown[];
          orgRefs: unknown[];
          images: unknown[];
          access: string;
        };
      }) => {
        const target = window as unknown as {
          __createdTask?: unknown;
          __createdTaskDraft?: unknown;
        };
        target.__createdTaskDraft = args.draft;
        const task = {
          id: `${args.draft.orgId}:22222222-2222-4222-8222-222222222299`,
          orgId: args.draft.orgId,
          docId: "22222222-2222-4222-8222-222222222299",
          itemId: "33333333-3333-4333-8333-333333333399",
          sourceDocumentId: "55555555-5555-4555-8555-555555555555",
          version: 1,
          ...args.draft,
          createdAt: "2026-08-21T12:00:00Z",
          canEdit: true,
          canManage: true,
          localRefs: [],
          updatedAt: "2026-08-21T12:00:00Z",
        };
        target.__createdTask = task;
        return task;
      },
    },
    {
      org_list_statuses: ORGS,
      list_dashboards: [],
    },
  );

  await page.goto("/tasks/new");
  await page.getByLabel("Task title").fill("Launch shared Tasks");
  await page.getByLabel("Task description").fill("One encrypted org document.");
  await page.getByRole("combobox", { name: "Status" }).selectOption("inProgress");
  await page.getByRole("textbox", { name: "Due" }).fill("2026-08-29T15:30");
  await page.getByRole("button", { name: "Create task" }).click();

  await expect(page).toHaveURL(
    new RegExp(`/tasks/${ORG_ID}:22222222-2222-4222-8222-222222222299$`),
  );
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as {
            __createdTaskDraft?: { orgId?: string; status?: string; dueAt?: string | null };
          }).__createdTaskDraft ?? null,
      ),
    )
    .toMatchObject({
      orgId: ORG_ID,
      status: "inProgress",
      dueAt: new Date("2026-08-29T15:30").toISOString(),
    });
});
