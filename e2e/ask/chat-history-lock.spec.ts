import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";
import { mockNotes } from "../notes/mock-invoke";

test("locking an authored-note folder evicts loaded vault Ask titles, messages, and sources", async ({
  page,
}) => {
  await mockTauri(page, {
    // No shares on this folder, so the lock flow seals directly instead of opening its
    // lock×shares dialog. The flow probes this FIRST and fails closed, so leaving it to the
    // demo default put a dialog in front of the behaviour under test.
    folder_active_shares: () => ({ links: 0, users: 0, org: [] }),
    list_workspace_tree: () => {
      const locked = Boolean(
        (window as unknown as { __historyNoteLocked?: boolean }).__historyNoteLocked,
      );
      return [
        {
          id: "p-root",
          name: "Workspace",
          level: "project",
          emoji: null,
          tint: null,
          locked: false,
          unlocked: false,
          isRoot: false,
          folders: [
            {
              id: "nf-history",
              name: "History note folder",
              level: "folder",
              emoji: null,
              tint: null,
              locked,
              unlocked: false,
              isRoot: false,
              folders: [],
              groups: [],
            },
          ],
          groups: [],
        },
      ];
    },
    list_link_candidates: () => [
      { kind: "note", id: "n-secret", title: "Sensitive source" },
    ],
    list_note_folders: () => {
      const locked = Boolean(
        (window as unknown as { __historyNoteLocked?: boolean })
          .__historyNoteLocked,
      );
      return [
        {
          id: "nf-history",
          name: "History note folder",
          path: "Notes/History",
          parentId: null,
          locked,
          unlocked: false,
          isRoot: false,
          kind: "note",
        },
      ];
    },
    lock_folder: (args: { folderId: string }) => {
      if (args.folderId === "nf-history") {
        (
          window as unknown as { __historyNoteLocked?: boolean }
        ).__historyNoteLocked = true;
      }
      // The real lock emits this — `emit_ask_history_invalidated_fail_closed` runs on every
      // lock-authority transition — and it is what scrubs mounted Ask plaintext. A mock that
      // stays silent describes a lock that does not do what a lock does.
      (globalThis as any).__demoEmit?.("murmur://ask-history-invalidated", null);
      return null;
    },
  });
  await page.goto("/ask");

  await page.locator("mur-source-picker .sp-trigger").click();
  await page.locator(".sp-row").first().click();
  await page.locator(".sp-scrim").click();
  await page.locator(".ask-input").fill("Sensitive Ask message");
  await page.locator(".ask-input").press("Enter");
  await expect(
    page.getByText("Sensitive Ask message", { exact: true }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Conversation history" }).click();
  await expect(
    page.getByText("Sensitive Ask message", { exact: true }),
  ).toBeVisible();

  // The hierarchy is on screen already, beside the mounted Ask route. The
  // loaded conversation and its sensitive source must remain present until
  // locking publishes the invalidation that scrubs them.
  await expect(page).toHaveURL(/\/ask$/);
  await expect(page.locator("mur-chat-history")).toBeVisible();
  await expect(
    page.getByText("Sensitive Ask message", { exact: true }),
  ).toBeVisible();

  // The Workspaces tree is a section of the one sidebar now, not a separate
  // "Workspaces sidebar" panel that had to be opened first.
  const spacesSidebar = page.getByRole("navigation", {
    name: "Primary navigation",
  });
  await expect(spacesSidebar).toBeVisible();
  await spacesSidebar
    .getByRole("button", { name: "Expand Workspace" })
    .click();
  await spacesSidebar
    .getByRole("button", { name: "Actions for History note folder" })
    .focus();
  await page.keyboard.press("Enter");
  const lock = page.getByRole("menuitem", { name: /^Lock (folder|project)/ });
  await expect(lock).toBeAttached();
  await lock.click({ force: true });

  await expect(page.locator("mur-chat-history")).toHaveCount(0);
  await expect(
    page.getByText("Sensitive Ask message", { exact: true }),
  ).toHaveCount(0);
  await expect(page.locator(".ask-row")).toHaveCount(0);
  await expect(page.locator("mur-source-picker .sp-chip")).toHaveCount(0);
});

test("locking a meeting folder evicts loaded vault Ask plaintext", async ({
  page,
}) => {
  await mockTauri(page, {
    // No shares on this folder, so the lock flow seals directly instead of opening its
    // lock×shares dialog. The flow probes this FIRST and fails closed, so leaving it to the
    // demo default put a dialog in front of the behaviour under test.
    folder_active_shares: () => ({ links: 0, users: 0, org: [] }),
    list_workspace_tree: () => {
      const locked = Boolean(
        (window as unknown as { __historyMeetingLocked?: boolean }).__historyMeetingLocked,
      );
      return [
        {
          id: "p-root",
          name: "Workspace",
          level: "project",
          emoji: null,
          tint: null,
          locked: false,
          unlocked: false,
          isRoot: false,
          folders: [
            {
              id: "f-history",
              name: "History meeting folder",
              level: "folder",
              emoji: null,
              tint: null,
              locked,
              unlocked: false,
              isRoot: false,
              folders: [],
              groups: [],
            },
          ],
          groups: [],
        },
      ];
    },
    list_folders: () => {
      const locked = Boolean(
        (window as unknown as { __historyMeetingLocked?: boolean })
          .__historyMeetingLocked,
      );
      return [
        {
          id: "f-history",
          name: "History meetings",
          parentId: null,
          noteCount: 1,
          locked,
          unlocked: false,
          children: [],
        },
      ];
    },
    lock_folder: (args: { folderId: string }) => {
      if (args.folderId === "f-history") {
        (
          window as unknown as { __historyMeetingLocked?: boolean }
        ).__historyMeetingLocked = true;
      }
      (globalThis as any).__demoEmit?.("murmur://ask-history-invalidated", null);
      return null;
    },
  });
  await page.goto("/ask");
  await page.locator(".ask-input").fill("Meeting-derived secret");
  await page.locator(".ask-input").press("Enter");
  await expect(
    page.getByText("Meeting-derived secret", { exact: true }),
  ).toBeVisible();

  // The Workspaces tree sits beside Ask: it must not unmount Ask or scrub the
  // message before the actual lock transition occurs.
  await expect(page).toHaveURL(/\/ask$/);
  await expect(
    page.getByText("Meeting-derived secret", { exact: true }),
  ).toBeVisible();

  // The Workspaces tree is a section of the one sidebar now, not a separate
  // "Workspaces sidebar" panel that had to be opened first.
  const spacesSidebar = page.getByRole("navigation", {
    name: "Primary navigation",
  });
  await expect(spacesSidebar).toBeVisible();
  await spacesSidebar
    .getByRole("button", { name: "Expand Workspace" })
    .click();
  await spacesSidebar
    .getByRole("button", { name: "Actions for History meeting folder" })
    .focus();
  await page.keyboard.press("Enter");
  const lock = page.getByRole("menuitem", { name: /^Lock (folder|project)/ });
  await expect(lock).toBeAttached();
  await lock.click({ force: true });
  await expect(
    page.getByText("Meeting-derived secret", { exact: true }),
  ).toHaveCount(0);
  await expect(page.locator(".ask-row")).toHaveCount(0);
});

test("a content deletion event evicts the durable Ask cache immediately", async ({
  page,
}) => {
  await mockTauri(page);
  await page.goto("/ask");
  await page.locator(".ask-input").fill("Deleted-source secret");
  await page.locator(".ask-input").press("Enter");
  await expect(
    page.getByText("Deleted-source secret", { exact: true }),
  ).toBeVisible();

  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://content-deleted", {
      kind: "note",
      id: "n-deleted",
    });
  });
  await expect(
    page.getByText("Deleted-source secret", { exact: true }),
  ).toHaveCount(0);
  await expect(page.locator(".ask-row")).toHaveCount(0);
});

test("the canonical Ask-history purge event evicts every mounted plaintext cache", async ({
  page,
}) => {
  await mockTauri(page, {
    list_link_candidates: () => [
      { kind: "note", id: "n-purge", title: "Purge-only source" },
    ],
  });
  await page.goto("/ask");

  await page.locator("mur-source-picker .sp-trigger").click();
  await page.locator(".sp-row").first().click();
  await page.locator(".sp-scrim").click();
  await page.locator(".ask-input").fill("Moved-source secret");
  await page.locator(".ask-input").press("Enter");
  await expect(
    page.getByText("Moved-source secret", { exact: true }),
  ).toBeVisible();
  await expect(page.locator("mur-source-picker .sp-chip")).toHaveCount(1);

  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://ask-history-invalidated", null);
  });

  await expect(
    page.getByText("Moved-source secret", { exact: true }),
  ).toHaveCount(0);
  await expect(page.locator(".ask-row")).toHaveCount(0);
  await expect(page.locator("mur-source-picker .sp-chip")).toHaveCount(0);
  await expect(page.locator("mur-chat-history")).toHaveCount(0);
});

test("privacy invalidation closes the source picker, scrubs cached labels, and drops a late fetch", async ({
  page,
}) => {
  await mockTauri(page, {
    list_dashboards: () => {
      const target = window as unknown as {
        __dashboardListCall?: number;
        __resolveLateDashboards?: () => void;
      };
      target.__dashboardListCall = (target.__dashboardListCall ?? 0) + 1;
      if (target.__dashboardListCall === 1) {
        return [
          {
            id: "dashboard-secret",
            title: "Sealed dashboard title",
            emoji: "🔒",
            tileCount: 2,
            createdAt: "2026-08-06T01:00:00Z",
            updatedAt: "2026-08-06T01:00:00Z",
          },
        ];
      }
      return new Promise((resolve) => {
        target.__resolveLateDashboards = () =>
          resolve([
            {
              id: "late-dashboard-secret",
              title: "Late sealed dashboard title",
              emoji: "🤫",
              tileCount: 1,
              createdAt: "2026-08-06T01:00:00Z",
              updatedAt: "2026-08-06T01:00:00Z",
            },
          ]);
      });
    },
    list_link_candidates: () =>
      new Promise((resolve) => {
        (
          window as unknown as {
            __resolveLatePrivateCandidates?: () => void;
          }
        ).__resolveLatePrivateCandidates = () =>
          resolve([
            {
              kind: "note",
              id: "late-sealed-note",
              title: "Late sealed source title",
            },
          ]);
      }),
    ask_vault_persisted: () =>
      new Promise((resolve) => {
        (
          window as unknown as {
            __resolveLatePrivateAsk?: () => void;
          }
        ).__resolveLatePrivateAsk = () =>
          resolve({
            conversationId: "stale-private-thread",
            userMessageId: crypto.randomUUID(),
            assistantMessageId: crypto.randomUUID(),
            answer: "Late sealed answer",
            sources: [],
            citations: [],
          });
      }),
  });
  await page.goto("/ask");

  await page.locator("mur-source-picker .sp-trigger").click();
  await page
    .getByRole("option", { name: "Use dashboard Sealed dashboard title" })
    .click();
  await expect(
    page.locator('[data-testid="selected-dashboard-chip"]'),
  ).toContainText("Sealed dashboard title");
  await page.locator(".ask-input").fill("Private held question");
  await page.locator(".ask-input").press("Enter");
  await expect(page.locator(".ask-typing")).toBeVisible();

  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://ask-history-invalidated", null);
  });

  await expect(page.locator(".sp-pop")).toHaveCount(0);
  await expect(
    page.getByText("Sealed dashboard title", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.locator('[data-testid="selected-dashboard-chip"]'),
  ).toHaveCount(0);
  await expect(
    page.getByText("Private held question", { exact: true }),
  ).toHaveCount(0);

  await page.locator("mur-source-picker .sp-trigger").click();
  await expect
    .poll(() =>
      page.evaluate(() =>
        Boolean(
          (
            window as unknown as {
              __resolveLateDashboards?: () => void;
            }
          ).__resolveLateDashboards,
        ),
      ),
    )
    .toBe(true);
  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://ask-history-invalidated", null);
  });
  await page.evaluate(() => {
    const target = window as unknown as {
      __resolveLatePrivateCandidates?: () => void;
      __resolveLateDashboards?: () => void;
      __resolveLatePrivateAsk?: () => void;
    };
    target.__resolveLatePrivateCandidates?.();
    target.__resolveLateDashboards?.();
    target.__resolveLatePrivateAsk?.();
  });
  await expect(
    page.getByText("Late sealed source title", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("Late sealed dashboard title", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("Late sealed answer", { exact: true }),
  ).toHaveCount(0);
  await expect(page.locator("mur-source-picker .sp-chip")).toHaveCount(0);
});

test("meeting drawer purge clears saved titles and sources and defeats a late prefill", async ({
  page,
}) => {
  await mockTauri(page, {
    list_links: () =>
      new Promise((resolve) => {
        (
          window as unknown as { __resolveLateMeetingLinks?: () => void }
        ).__resolveLateMeetingLinks = () =>
          resolve([
            {
              id: 91,
              direction: "out",
              otherKind: "note",
              otherId: "n-late-secret",
              otherTitle: "Late meeting-linked secret",
              edgeType: "companion",
              createdBy: "auto",
              status: "active",
              score: 0,
              createdAt: 0,
            },
          ]);
      }),
    list_ask_conversations: () => [
      {
        id: "meeting-secret-thread",
        scope: { kind: "meeting", refId: "m-atlas-roadmap" },
        title: "Meeting history secret title",
        createdAt: "2026-08-06T01:00:00Z",
        updatedAt: "2026-08-06T01:01:00Z",
        messageCount: 2,
      },
    ],
    load_ask_conversation: () => ({
      id: "meeting-secret-thread",
      scope: { kind: "meeting", refId: "m-atlas-roadmap" },
      title: "Meeting history secret title",
      selectedSources: [
        {
          kind: "note",
          id: "n-saved-secret",
          title: "Saved meeting secret source",
        },
      ],
      messages: [
        {
          id: "00000000-0000-4000-8000-000000000201",
          ordinal: 0,
          role: "user",
          content: "Meeting drawer secret message",
          sources: [],
          citations: [],
          createdAt: "2026-08-06T01:00:00Z",
        },
        {
          id: "00000000-0000-4000-8000-000000000202",
          ordinal: 1,
          role: "assistant",
          content: "Meeting drawer secret answer",
          sources: [],
          citations: [],
          createdAt: "2026-08-06T01:01:00Z",
        },
      ],
      createdAt: "2026-08-06T01:00:00Z",
      updatedAt: "2026-08-06T01:01:00Z",
    }),
  });
  await page.goto("/meeting/m-atlas-roadmap");
  await page.getByRole("button", { name: /Ask/ }).click();
  const chat = page.locator("app-meeting-chat");
  await chat.getByRole("button", { name: "Conversation history" }).click();
  await chat.locator(".history-row").click();
  await expect(
    chat.getByText("Meeting drawer secret message", { exact: true }),
  ).toBeVisible();
  await expect(chat.locator(".sp-chip-title")).toHaveText([
    "Saved meeting secret source",
  ]);
  await chat.getByRole("button", { name: "Conversation history" }).click();
  await expect(
    chat.getByText("Meeting history secret title", { exact: true }),
  ).toBeVisible();

  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://ask-history-invalidated", null);
  });

  await expect(
    chat.getByText("Meeting history secret title", { exact: true }),
  ).toHaveCount(0);
  await expect(
    chat.getByText("Meeting drawer secret message", { exact: true }),
  ).toHaveCount(0);
  await expect(chat.locator(".sp-chip-title")).toHaveCount(0);
  await page.evaluate(() => {
    (
      window as unknown as { __resolveLateMeetingLinks?: () => void }
    ).__resolveLateMeetingLinks?.();
  });
  await page.waitForTimeout(100);
  await expect(chat.locator(".sp-chip-title")).toHaveCount(0);
  await expect(
    chat.getByText("Late meeting-linked secret", { exact: true }),
  ).toHaveCount(0);
  await chat.getByRole("button", { name: "New conversation" }).click();
  await expect(chat.locator(".sp-chip-title")).toHaveCount(0);
  await expect(
    chat.getByText("Late meeting-linked secret", { exact: true }),
  ).toHaveCount(0);
});

test("authored-note drawer purge clears saved titles and sources and defeats a late prefill", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: () =>
      new Promise((resolve) => {
        (
          window as unknown as { __resolveLateNotePurgeLinks?: () => void }
        ).__resolveLateNotePurgeLinks = () =>
          resolve([
            {
              id: 92,
              direction: "out",
              otherKind: "meeting",
              otherId: "m-late-secret",
              otherTitle: "Late note-linked secret",
              edgeType: "wikilink",
              createdBy: "auto",
              status: "active",
              score: 0,
              createdAt: 0,
            },
          ]);
      }),
    list_ask_conversations: () => [
      {
        id: "note-secret-thread",
        scope: { kind: "note", refId: "n1" },
        title: "Note history secret title",
        createdAt: "2026-08-06T01:00:00Z",
        updatedAt: "2026-08-06T01:01:00Z",
        messageCount: 2,
      },
    ],
    load_ask_conversation: () => ({
      id: "note-secret-thread",
      scope: { kind: "note", refId: "n1" },
      title: "Note history secret title",
      selectedSources: [
        {
          kind: "meeting",
          id: "m-saved-secret",
          title: "Saved note secret source",
        },
      ],
      messages: [
        {
          id: "00000000-0000-4000-8000-000000000301",
          ordinal: 0,
          role: "user",
          content: "Note drawer secret message",
          sources: [],
          citations: [],
          createdAt: "2026-08-06T01:00:00Z",
        },
        {
          id: "00000000-0000-4000-8000-000000000302",
          ordinal: 1,
          role: "assistant",
          content: "Note drawer secret answer",
          sources: [],
          citations: [],
          createdAt: "2026-08-06T01:01:00Z",
        },
      ],
      createdAt: "2026-08-06T01:00:00Z",
      updatedAt: "2026-08-06T01:01:00Z",
    }),
  });
  await page.goto("/notes/n1");
  await page.locator(".head-chat-btn").click();
  const chat = page.locator(".note-chat-drawer app-note-chat");
  await chat.getByRole("button", { name: "Conversation history" }).click();
  await chat.locator(".history-row").click();
  await expect(
    chat.getByText("Note drawer secret message", { exact: true }),
  ).toBeVisible();
  await expect(chat.locator(".sp-chip-title")).toHaveText([
    "Saved note secret source",
  ]);
  await chat.getByRole("button", { name: "Conversation history" }).click();
  await expect(
    chat.getByText("Note history secret title", { exact: true }),
  ).toBeVisible();

  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://ask-history-invalidated", null);
  });

  await expect(
    chat.getByText("Note history secret title", { exact: true }),
  ).toHaveCount(0);
  await expect(
    chat.getByText("Note drawer secret message", { exact: true }),
  ).toHaveCount(0);
  await expect(chat.locator(".sp-chip-title")).toHaveCount(0);
  await page.evaluate(() => {
    (
      window as unknown as { __resolveLateNotePurgeLinks?: () => void }
    ).__resolveLateNotePurgeLinks?.();
  });
  await page.waitForTimeout(100);
  await expect(chat.locator(".sp-chip-title")).toHaveCount(0);
  await expect(
    chat.getByText("Late note-linked secret", { exact: true }),
  ).toHaveCount(0);
  await chat.getByRole("button", { name: "New conversation" }).click();
  await expect(chat.locator(".sp-chip-title")).toHaveCount(0);
  await expect(
    chat.getByText("Late note-linked secret", { exact: true }),
  ).toHaveCount(0);
});

test("durable Ask cannot read or send before every privacy listener is acknowledged", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      list_ask_conversations: () => {
        const state = window as unknown as { __privacyListCalls?: number };
        state.__privacyListCalls = (state.__privacyListCalls ?? 0) + 1;
        return [
          {
            id: "barrier-thread",
            scope: { kind: "vault" },
            title: "Listener barrier secret",
            createdAt: "2026-08-06T01:00:00Z",
            updatedAt: "2026-08-06T01:01:00Z",
            messageCount: 2,
          },
        ];
      },
      ask_vault_persisted: () => {
        const state = window as unknown as { __privacySendCalls?: number };
        state.__privacySendCalls = (state.__privacySendCalls ?? 0) + 1;
        return {
          conversationId: "barrier-thread",
          userMessageId: crypto.randomUUID(),
          assistantMessageId: crypto.randomUUID(),
          answer: "must not dispatch before the barrier",
          sources: [],
          citations: [],
        };
      },
    },
    {},
    [],
    ["murmur://ask-history-invalidated"],
  );
  await page.goto("/ask");

  const history = page.getByRole("button", { name: "Conversation history" });
  await expect(history).toBeDisabled();
  await expect(page.locator(".ask-input")).toBeDisabled();
  expect(
    await page.evaluate(() => ({
      list:
        (window as unknown as { __privacyListCalls?: number })
          .__privacyListCalls ?? 0,
      send:
        (window as unknown as { __privacySendCalls?: number })
          .__privacySendCalls ?? 0,
    })),
  ).toEqual({ list: 0, send: 0 });

  // This event is deliberately not replayed when the delayed listener finally
  // registers. Safety comes from refusing all content work before the ACK.
  await page.evaluate(() => {
    const target = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
      __demoReleaseEventListeners: (event: string) => void;
    };
    target.__demoEmit("murmur://ask-history-invalidated", null);
    target.__demoReleaseEventListeners("murmur://ask-history-invalidated");
  });

  await expect(history).toBeEnabled();
  await history.click();
  await expect(
    page.getByText("Listener barrier secret", { exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __privacyListCalls?: number })
          .__privacyListCalls ?? 0,
    ),
  ).toBe(1);
});

test("privacy-listener failure is fail-closed and double Retry coalesces one recovery", async ({
  page,
}) => {
  const askEvent = "murmur://ask-history-invalidated";
  const contentEvent = "murmur://content-deleted";
  const visibilityEvent = "murmur://reminder-visibility-invalidated";
  await mockTauri(
    page,
    {
      list_ask_conversations: () => {
        const state = window as unknown as { __privacyRetryLists?: number };
        state.__privacyRetryLists = (state.__privacyRetryLists ?? 0) + 1;
        return [];
      },
      ask_vault_persisted: () => {
        const state = window as unknown as { __privacyRetrySends?: number };
        state.__privacyRetrySends = (state.__privacyRetrySends ?? 0) + 1;
        return {
          conversationId: "must-not-send",
          userMessageId: crypto.randomUUID(),
          assistantMessageId: crypto.randomUUID(),
          answer: "must not send",
          sources: [],
          citations: [],
        };
      },
      list_link_candidates: () => {
        const state = window as unknown as { __privacySourceReads?: number };
        state.__privacySourceReads = (state.__privacySourceReads ?? 0) + 1;
        return [];
      },
    },
    {},
    [],
    [],
    { [askEvent]: [2] },
    { [askEvent]: [1] },
  );
  await page.goto("/ask");

  const secureAlert = page.getByRole("alert").filter({
    hasText: "Ask Brain isn’t available securely right now.",
  });
  await expect(secureAlert).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Conversation history" }),
  ).toBeDisabled();
  await expect(page.locator(".ask-input")).toBeDisabled();
  expect(
    await page.evaluate(() => ({
      list:
        (window as unknown as { __privacyRetryLists?: number })
          .__privacyRetryLists ?? 0,
      send:
        (window as unknown as { __privacyRetrySends?: number })
          .__privacyRetrySends ?? 0,
      sources:
        (window as unknown as { __privacySourceReads?: number })
          .__privacySourceReads ?? 0,
    })),
  ).toEqual({ list: 0, send: 0, sources: 0 });

  const beforeRetry = await page.evaluate(
    ([ask, content, visibility]) => {
      const count = (
        window as unknown as {
          __demoEventListenerRegistrationCount: (event: string) => number;
        }
      ).__demoEventListenerRegistrationCount;
      return {
        ask: count(ask),
        content: count(content),
        visibility: count(visibility),
      };
    },
    [askEvent, contentEvent, visibilityEvent],
  );
  await secureAlert
    .getByRole("button", { name: "Retry" })
    .evaluate((button) => {
      button.click();
      button.click();
    });
  await expect
    .poll(() =>
      page.evaluate(
        (event) =>
          (
            window as unknown as {
              __demoEventListenerRegistrationCount: (name: string) => number;
            }
          ).__demoEventListenerRegistrationCount(event),
        askEvent,
      ),
    )
    .toBe(beforeRetry.ask + 1);
  const duringRetry = await page.evaluate(
    ([ask, content, visibility]) => {
      const count = (
        window as unknown as {
          __demoEventListenerRegistrationCount: (event: string) => number;
        }
      ).__demoEventListenerRegistrationCount;
      return {
        ask: count(ask),
        content: count(content),
        visibility: count(visibility),
      };
    },
    [askEvent, contentEvent, visibilityEvent],
  );
  expect(duringRetry).toEqual({
    ask: beforeRetry.ask + 1,
    content: beforeRetry.content,
    visibility: beforeRetry.visibility,
  });

  await page.evaluate((event) => {
    (
      window as unknown as {
        __demoReleaseEventListeners: (name: string) => void;
      }
    ).__demoReleaseEventListeners(event);
  }, askEvent);
  await expect(secureAlert).toHaveCount(0);
  const history = page.getByRole("button", { name: "Conversation history" });
  await expect(history).toBeEnabled();
  await history.click();
  await expect(page.getByText("No conversations yet")).toBeVisible();
});
