import { expect, test, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";
import { enterEditMode } from "../notes/mock-invoke";

/**
 * Browse is a disclosure group inside the ONE sidebar now — it used to be a
 * separate "Browse sidebar" complementary panel — and it starts collapsed, so
 * the Reminders destination (and its unread `.count`) is not in the DOM until
 * the group is opened. Idempotent, so callers that already opened it are safe.
 */
async function openBrowse(page: Page) {
  const browse = page.getByRole("navigation", { name: "Browse destinations" });
  const toggle = browse.getByRole("button", { name: "Browse", exact: true });
  if ((await toggle.getAttribute("aria-expanded")) !== "true") {
    await toggle.click();
  }
  return browse;
}

const REMINDER_VISIBILITY_EVENT = "murmur://reminder-visibility-invalidated";
const REMINDERS_UPDATED_EVENT = "murmur://reminders-updated";

test("Reminders: a live count cannot be lost or overwritten by a stale startup summary", async ({
  page,
}) => {
  await mockTauri(page, {
    get_reminder_summary: () => {
      const target = window as unknown as {
        __summaryRequestStarted?: boolean;
        __resolveReminderSummary?: (value: { dueInboxCount: number }) => void;
      };
      target.__summaryRequestStarted = true;
      return new Promise<{ dueInboxCount: number }>((resolve) => {
        target.__resolveReminderSummary = resolve;
      });
    },
  });

  await page.goto("/record");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __summaryRequestStarted?: boolean })
            .__summaryRequestStarted === true,
      ),
    )
    .toBe(true);

  await page.evaluate(() => {
    const target = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
      __resolveReminderSummary?: (value: { dueInboxCount: number }) => void;
    };
    target.__demoEmit("murmur://reminders-updated", { dueInboxCount: 4 });
    target.__resolveReminderSummary?.({ dueInboxCount: 2 });
  });

  const reminderNav = (await openBrowse(page)).getByRole("link", {
    name: "Reminders",
  });
  await expect(reminderNav.locator(".count")).toHaveText("4");
});

test("Reminders: a listener resolving after root teardown is immediately unregistered", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {},
    [],
    [REMINDERS_UPDATED_EVENT],
  );

  await page.goto("/reminders");
  await expect
    .poll(() =>
      page.evaluate(
        (event) =>
          (
            window as unknown as {
              __demoEventListenerRegistrationCount: (
                name: string,
              ) => number;
            }
          ).__demoEventListenerRegistrationCount(event),
        REMINDERS_UPDATED_EVENT,
      ),
    )
    .toBe(1);
  await expect
    .poll(() =>
      page.evaluate(
        (event) =>
          (
            window as unknown as {
              __demoEventListenerUnregisterCount: (name: string) => number;
            }
          ).__demoEventListenerUnregisterCount(event),
        REMINDERS_UPDATED_EVENT,
      ),
    )
    .toBe(0);

  await page.evaluate(() => {
    type AngularDebug = {
      getInjector: (element: Element) => unknown;
      ɵgetInjectorMetadata: (injector: unknown) => { type?: string } | null;
      ɵgetInjectorResolutionPath: (injector: unknown) => unknown[];
    };
    const angular = (window as unknown as { ng?: AngularDebug }).ng;
    const root = document.querySelector("app-root");
    if (!angular || !root) {
      throw new Error("Angular debug injector is unavailable");
    }
    const rootInjector = angular.getInjector(root);
    const appEnvironment = angular
      .ɵgetInjectorResolutionPath(rootInjector)
      .find(
        (injector) =>
          angular.ɵgetInjectorMetadata(injector)?.type === "environment" &&
          typeof (injector as { destroy?: unknown }).destroy === "function",
      ) as { destroy: () => void } | undefined;
    if (!appEnvironment) {
      throw new Error("Angular application environment injector is unavailable");
    }
    appEnvironment.destroy();
  });

  await page.evaluate((event) => {
    (
      window as unknown as {
        __demoReleaseEventListeners: (name: string) => void;
      }
    ).__demoReleaseEventListeners(event);
  }, REMINDERS_UPDATED_EVENT);

  await expect
    .poll(() =>
      page.evaluate(
        (event) =>
          (
            window as unknown as {
              __demoEventListenerUnregisterCount: (name: string) => number;
            }
          ).__demoEventListenerUnregisterCount(event),
        REMINDERS_UPDATED_EVENT,
      ),
    )
    .toBe(1);
});

test("Reminders: an event supersedes the first in-flight list snapshot", async ({
  page,
}) => {
  await mockTauri(page, {
    get_reminder_summary: () => {
      const target = window as unknown as Record<string, any>;
      target.__firstListSummaryStarted = true;
      return { dueInboxCount: 0 };
    },
    list_reminders: () => {
      const target = window as unknown as Record<string, any>;
      const makeSnapshot = (title: string, count: number) => {
        const now = Date.now();
        return {
          inbox: Array.from({ length: count }, (_, index) => ({
            occurrenceId: `${title}-occurrence-${index}`,
            dueAt: now - (index + 1) * 60_000,
            reminder: {
              id: `${title}-reminder-${index}`,
              title: `${title} ${index + 1}`,
              details: null,
              dueAt: now - (index + 1) * 60_000,
              repeatEvery: null,
              repeatUnit: null,
              state: "active",
              origin: "manual",
              createdAt: now - 120_000,
              updatedAt: now - 120_000,
              completedAt: null,
              sources: [],
            },
          })),
          upcoming: [],
          completed: [],
          dueInboxCount: count,
        };
      };

      target.__firstListCalls = (target.__firstListCalls ?? 0) + 1;
      if (target.__firstListCalls === 1) {
        target.__firstReminderListStarted = true;
        const staleSnapshot = makeSnapshot("Stale first row", 1);
        return new Promise((resolve) => {
          target.__resolveFirstReminderList = () => resolve(staleSnapshot);
        });
      }
      return makeSnapshot("Current event row", 4);
    },
  });

  await page.goto("/reminders");
  await expect
    .poll(() =>
      page.evaluate(() => {
        const target = window as unknown as Record<string, any>;
        return (
          target.__firstListSummaryStarted === true &&
          target.__firstReminderListStarted === true
        );
      }),
    )
    .toBe(true);

  await page.evaluate(() => {
    const target = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__demoEmit("murmur://reminders-updated", { dueInboxCount: 4 });
  });

  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as Record<string, any>).__firstListCalls ?? 0,
      ),
    )
    .toBe(2);
  await expect(page.getByText("Current event row 1")).toBeVisible();

  await page.evaluate(async () => {
    const target = window as unknown as Record<string, any>;
    target.__resolveFirstReminderList?.();
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
    });
  });

  await expect(page.getByText("Current event row 1")).toBeVisible();
  await expect(page.getByText("Stale first row 1")).toHaveCount(0);
  await expect(
    (await openBrowse(page))
      .getByRole("link", { name: "Reminders" })
      .locator(".count"),
  ).toHaveText("4");
});

test("Reminders: source invalidation masks a cached title before canonical refresh resolves", async ({
  page,
}) => {
  await mockTauri(page, {
    get_reminder_summary: () => ({ dueInboxCount: 1 }),
    list_reminders: () => {
      const target = window as unknown as {
        __sourceMaskListCalls?: number;
        __sourceMaskRefreshStarted?: boolean;
        __resolveSourceMaskRefresh?: () => void;
      };
      const now = Date.now();
      const reminder = {
        id: "r-source-mask",
        title: "Keep this reminder",
        details: null,
        dueAt: now - 60_000,
        repeatEvery: null,
        repeatUnit: null,
        state: "active",
        origin: "manual",
        createdAt: now - 120_000,
        updatedAt: now - 120_000,
        completedAt: null,
        sources: [
          {
            kind: "meeting",
            id: "m-source-mask",
            title: "Sealed source title",
          },
        ],
      };
      target.__sourceMaskListCalls = (target.__sourceMaskListCalls ?? 0) + 1;
      if (target.__sourceMaskListCalls === 1) {
        return {
          inbox: [
            {
              occurrenceId: "o-source-mask",
              dueAt: reminder.dueAt,
              reminder,
            },
          ],
          upcoming: [],
          completed: [],
          dueInboxCount: 1,
        };
      }
      target.__sourceMaskRefreshStarted = true;
      return new Promise((resolve) => {
        target.__resolveSourceMaskRefresh = () =>
          resolve({
            inbox: [
              {
                occurrenceId: "o-source-mask",
                dueAt: reminder.dueAt,
                reminder: { ...reminder, sources: [] },
              },
            ],
            upcoming: [],
            completed: [],
            dueInboxCount: 1,
          });
      });
    },
  });

  await page.goto("/reminders");
  const sourceChip = page
    .locator(".source-chip")
    .filter({ hasText: "Sealed source title" });
  await expect(sourceChip).toBeVisible();

  await page.evaluate(() => {
    const target = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__demoEmit("murmur://reminder-source-updated", {
      kind: "meeting",
      id: "m-source-mask",
    });
  });

  // The canonical read is deliberately still pending: masking must be a
  // synchronous cache operation, not a successful-refetch side effect.
  await expect(sourceChip).toHaveCount(0, { timeout: 700 });
  await expect(page.getByText("Keep this reminder")).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __sourceMaskRefreshStarted?: boolean;
            }
          ).__sourceMaskRefreshStarted === true,
      ),
    )
    .toBe(true);

  await page.evaluate(() => {
    (
      window as unknown as {
        __resolveSourceMaskRefresh?: () => void;
      }
    ).__resolveSourceMaskRefresh?.();
  });
  await expect(page.getByText("Keep this reminder")).toBeVisible();
  await expect(sourceChip).toHaveCount(0);
});

test("Reminders: source invalidation racing the first list cannot restore a stale title", async ({
  page,
}) => {
  await mockTauri(page, {
    get_reminder_summary: () => ({ dueInboxCount: 1 }),
    list_reminders: () => {
      const target = window as unknown as {
        __sourceRaceListCalls?: number;
        __sourceRaceFirstStarted?: boolean;
        __resolveSourceRaceFirst?: () => void;
      };
      const now = Date.now();
      target.__sourceRaceListCalls = (target.__sourceRaceListCalls ?? 0) + 1;
      const makeSnapshot = (title: string, withSource: boolean) => {
        const reminder = {
          id: "r-source-race",
          title,
          details: null,
          dueAt: now - 60_000,
          repeatEvery: null,
          repeatUnit: null,
          state: "active",
          origin: "manual",
          createdAt: now - 120_000,
          updatedAt: now - 120_000,
          completedAt: null,
          sources: withSource
            ? [
                {
                  kind: "note",
                  id: "n-source-race",
                  title: "First snapshot sealed title",
                },
              ]
            : [],
        };
        return {
          inbox: [
            {
              occurrenceId: "o-source-race",
              dueAt: reminder.dueAt,
              reminder,
            },
          ],
          upcoming: [],
          completed: [],
          dueInboxCount: 1,
        };
      };
      if (target.__sourceRaceListCalls === 1) {
        target.__sourceRaceFirstStarted = true;
        const stale = makeSnapshot("Stale reminder snapshot", true);
        return new Promise((resolve) => {
          target.__resolveSourceRaceFirst = () => resolve(stale);
        });
      }
      return makeSnapshot("Current reminder snapshot", false);
    },
  });

  await page.goto("/reminders");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __sourceRaceFirstStarted?: boolean;
            }
          ).__sourceRaceFirstStarted === true,
      ),
    )
    .toBe(true);

  await page.evaluate(() => {
    const target = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__demoEmit("murmur://reminder-source-updated", {
      kind: "note",
      id: "n-source-race",
    });
  });

  await expect(page.getByText("Current reminder snapshot")).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __sourceRaceListCalls?: number;
            }
          ).__sourceRaceListCalls ?? 0,
      ),
    )
    .toBe(2);

  await page.evaluate(async () => {
    const target = window as unknown as {
      __resolveSourceRaceFirst?: () => void;
    };
    target.__resolveSourceRaceFirst?.();
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
    });
  });

  await expect(page.getByText("Current reminder snapshot")).toBeVisible();
  await expect(page.getByText("Stale reminder snapshot")).toHaveCount(0);
  await expect(
    page.getByText("First snapshot sealed title", { exact: true }),
  ).toHaveCount(0);
});

test("Reminder composer: source invalidation closes and purges an open edit", async ({
  page,
}) => {
  await mockTauri(page, {
    get_reminder_summary: () => ({ dueInboxCount: 1 }),
    list_reminders: () => {
      const target = window as unknown as {
        __composerSourceListCalls?: number;
      };
      const now = Date.now();
      target.__composerSourceListCalls =
        (target.__composerSourceListCalls ?? 0) + 1;
      const reminder = {
        id: "r-composer-source",
        title: "Composer reminder",
        details: null,
        dueAt: now - 60_000,
        repeatEvery: null,
        repeatUnit: null,
        state: "active",
        origin: "manual",
        createdAt: now - 120_000,
        updatedAt: now - 120_000,
        completedAt: null,
        sources:
          target.__composerSourceListCalls === 1
            ? [
                {
                  kind: "meeting",
                  id: "m-composer-source",
                  title: "Composer sealed title",
                },
              ]
            : [],
      };
      return {
        inbox: [
          {
            occurrenceId: "o-composer-source",
            dueAt: reminder.dueAt,
            reminder,
          },
        ],
        upcoming: [],
        completed: [],
        dueInboxCount: 1,
      };
    },
  });

  await page.goto("/reminders");
  await expect(page.getByText("Composer reminder")).toBeVisible();
  await page
    .locator(".reminder-card")
    .filter({ hasText: "Composer reminder" })
    .getByRole("button", { name: "Edit" })
    .click();

  const composer = page.locator("app-reminder-composer");
  await expect(composer.getByRole("dialog")).toBeVisible();
  await expect(
    composer.getByText("Composer sealed title", { exact: true }),
  ).toBeVisible();

  await page.evaluate(() => {
    const target = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__demoEmit("murmur://reminder-source-updated", {
      kind: "meeting",
      id: "m-composer-source",
    });
  });

  await expect(composer.getByRole("dialog")).toHaveCount(0, { timeout: 700 });
  await expect(
    page.getByText("Composer sealed title", { exact: true }),
  ).toHaveCount(0);
});

test("Reminders: global visibility invalidation purges every cached source and composer", async ({
  page,
}) => {
  await mockTauri(page, {
    get_reminder_summary: () => ({ dueInboxCount: 1 }),
    list_reminders: () => {
      const target = window as unknown as {
        __globalVisibilityListCalls?: number;
        __globalVisibilityRefreshStarted?: boolean;
        __resolveGlobalVisibilityRefresh?: () => void;
      };
      const now = Date.now();
      const reminder = (
        id: string,
        title: string,
        sourceTitle: string,
        dueAt: number,
        state: "active" | "completed",
      ) => ({
        id,
        title,
        details: null,
        dueAt,
        repeatEvery: null,
        repeatUnit: null,
        state,
        origin: "manual",
        createdAt: now - 120_000,
        updatedAt: now - 120_000,
        completedAt: state === "completed" ? now - 30_000 : null,
        sources: [
          {
            kind: "meeting",
            id: `m-${id}`,
            title: sourceTitle,
          },
        ],
      });
      const inboxReminder = reminder(
        "global-inbox",
        "Global inbox reminder",
        "Global inbox source",
        now - 60_000,
        "active",
      );
      const snapshot = {
        inbox: [
          {
            occurrenceId: "o-global-inbox",
            dueAt: inboxReminder.dueAt,
            reminder: inboxReminder,
          },
        ],
        upcoming: [
          reminder(
            "global-upcoming",
            "Global upcoming reminder",
            "Global upcoming source",
            now + 60_000,
            "active",
          ),
        ],
        completed: [
          reminder(
            "global-completed",
            "Global completed reminder",
            "Global completed source",
            now - 120_000,
            "completed",
          ),
        ],
        dueInboxCount: 1,
      };
      target.__globalVisibilityListCalls =
        (target.__globalVisibilityListCalls ?? 0) + 1;
      if (target.__globalVisibilityListCalls === 1) {
        return snapshot;
      }
      target.__globalVisibilityRefreshStarted = true;
      return new Promise((resolve) => {
        target.__resolveGlobalVisibilityRefresh = () =>
          resolve({
            ...snapshot,
            inbox: snapshot.inbox.map((row) => ({
              ...row,
              reminder: { ...row.reminder, sources: [] },
            })),
            upcoming: snapshot.upcoming.map((row) => ({
              ...row,
              sources: [],
            })),
            completed: snapshot.completed.map((row) => ({
              ...row,
              sources: [],
            })),
          });
      });
    },
  });

  await page.goto("/reminders");
  await expect(page.getByText("Global inbox source")).toBeVisible();
  await page
    .locator(".reminder-card")
    .filter({ hasText: "Global inbox reminder" })
    .getByRole("button", { name: "Edit" })
    .click();
  const composer = page.locator("app-reminder-composer");
  await expect(composer.getByRole("dialog")).toBeVisible();
  await expect(composer.getByText("Global inbox source")).toBeVisible();

  await page.evaluate(() => {
    const target = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__demoEmit("murmur://reminder-visibility-invalidated", null);
  });

  await expect(composer.getByRole("dialog")).toHaveCount(0, { timeout: 700 });
  await expect(page.getByText("Global inbox source")).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __globalVisibilityRefreshStarted?: boolean;
            }
          ).__globalVisibilityRefreshStarted === true,
      ),
    )
    .toBe(true);

  await page.getByRole("button", { name: /Upcoming/ }).click();
  await expect(page.getByText("Global upcoming reminder")).toBeVisible();
  await expect(page.getByText("Global upcoming source")).toHaveCount(0);
  await page.getByRole("button", { name: /Completed/ }).click();
  await expect(page.getByText("Global completed reminder")).toBeVisible();
  await expect(page.getByText("Global completed source")).toHaveCount(0);

  await page.evaluate(() => {
    (
      window as unknown as {
        __resolveGlobalVisibilityRefresh?: () => void;
      }
    ).__resolveGlobalVisibilityRefresh?.();
  });
  await expect(page.getByText("Global completed reminder")).toBeVisible();
});

test("Reminders: a newer list count beats a delayed startup summary", async ({
  page,
}) => {
  await mockTauri(page, {
    get_reminder_summary: () => {
      const target = window as unknown as Record<string, any>;
      target.__delayedSummaryStarted = true;
      return new Promise<{ dueInboxCount: number }>((resolve) => {
        target.__resolveDelayedSummary = resolve;
      });
    },
    list_reminders: () => {
      const target = window as unknown as Record<string, any>;
      const now = Date.now();
      const currentSnapshot = {
        inbox: Array.from({ length: 3 }, (_, index) => ({
          occurrenceId: `newest-occurrence-${index}`,
          dueAt: now - (index + 1) * 60_000,
          reminder: {
            id: `newest-reminder-${index}`,
            title: `Newest list row ${index + 1}`,
            details: null,
            dueAt: now - (index + 1) * 60_000,
            repeatEvery: null,
            repeatUnit: null,
            state: "active",
            origin: "manual",
            createdAt: now - 120_000,
            updatedAt: now - 120_000,
            completedAt: null,
            sources: [],
          },
        })),
        upcoming: [],
        completed: [],
        dueInboxCount: 3,
      };
      return new Promise((resolve) => {
        const resolveAfterSummaryStarted = () => {
          if (target.__delayedSummaryStarted === true) {
            resolve(currentSnapshot);
            return;
          }
          window.setTimeout(resolveAfterSummaryStarted, 0);
        };
        resolveAfterSummaryStarted();
      });
    },
  });

  await page.goto("/reminders");
  await expect(page.getByText("Newest list row 1")).toBeVisible();
  const reminderCount = (await openBrowse(page))
    .getByRole("link", { name: "Reminders" })
    .locator(".count");
  await expect(reminderCount).toHaveText("3");

  await page.evaluate(async () => {
    const target = window as unknown as Record<string, any>;
    target.__resolveDelayedSummary?.({ dueInboxCount: 2 });
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
    });
  });

  await expect(reminderCount).toHaveText("3");
});

test("Reminders: visibility-listener registration failure blocks source reads and composer hydration", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      get_reminder_summary: () => ({ dueInboxCount: 1 }),
      list_reminders: () => {
        const target = window as unknown as {
          __failedVisibilityListCalls?: number;
        };
        target.__failedVisibilityListCalls =
          (target.__failedVisibilityListCalls ?? 0) + 1;
        const now = Date.now();
        return {
          inbox: [
            {
              occurrenceId: "o-listener-failed",
              dueAt: now - 60_000,
              reminder: {
                id: "r-listener-failed",
                title: "Reminder survives",
                details: null,
                dueAt: now - 60_000,
                repeatEvery: null,
                repeatUnit: null,
                state: "active",
                origin: "manual",
                createdAt: now - 120_000,
                updatedAt: now - 120_000,
                completedAt: null,
                sources: [
                  {
                    kind: "meeting",
                    id: "m-listener-failed",
                    title: "Must never enter the renderer",
                  },
                ],
              },
            },
          ],
          upcoming: [],
          completed: [],
          dueInboxCount: 1,
        };
      },
    },
    {},
    [REMINDER_VISIBILITY_EVENT],
  );

  await page.goto("/reminders");
  await expect(
    page.getByText("Couldn’t load reminders. Please try again."),
  ).toBeVisible();
  await expect(page.getByText("Inbox clear")).toHaveCount(0);
  await expect(page.getByText("Must never enter the renderer")).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as {
            __failedVisibilityListCalls?: number;
          }
        ).__failedVisibilityListCalls ?? 0,
    ),
  ).toBe(0);

  await page.getByRole("button", { name: "New reminder" }).first().click();
  await expect(
    page.locator("app-reminder-composer").getByRole("dialog"),
  ).toHaveCount(0);
});

test("Reminders: Retry reconnects a transient privacy-listener failure without duplicates", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      get_reminder_summary: () => ({ dueInboxCount: 1 }),
      list_reminders: () => {
        const target = window as unknown as {
          __privacyRetryListCalls?: number;
          __privacyRetryLocked?: boolean;
        };
        target.__privacyRetryListCalls =
          (target.__privacyRetryListCalls ?? 0) + 1;
        const now = Date.now();
        const reminder = {
          id: "r-privacy-retry",
          title: "Retry-safe reminder",
          details: null,
          dueAt: now - 60_000,
          repeatEvery: null,
          repeatUnit: null,
          state: "active",
          origin: "manual",
          createdAt: now - 120_000,
          updatedAt: now - 120_000,
          completedAt: null,
          sources: target.__privacyRetryLocked
            ? []
            : [
                {
                  kind: "meeting",
                  id: "m-privacy-retry",
                  title: "Retry private title",
                },
              ],
        };
        return {
          inbox: [
            {
              occurrenceId: "o-privacy-retry",
              dueAt: reminder.dueAt,
              reminder,
            },
          ],
          upcoming: [],
          completed: [],
          dueInboxCount: 1,
        };
      },
    },
    {},
    [],
    [],
    {},
    {},
    {
      [REMINDER_VISIBILITY_EVENT]: Array.from(
        { length: 20 },
        (_, index) => index + 1,
      ),
    },
  );

  await page.goto("/reminders");
  await expect(
    page.getByText("Couldn’t load reminders. Please try again."),
  ).toBeVisible();
  await expect(page.getByText("Inbox clear")).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as {
            __privacyRetryListCalls?: number;
          }
        ).__privacyRetryListCalls ?? 0,
    ),
  ).toBe(0);

  await page.evaluate((event) => {
    (
      window as unknown as {
        __demoReleaseRejectedEventListeners: (name: string) => void;
      }
    ).__demoReleaseRejectedEventListeners(event);
  }, REMINDER_VISIBILITY_EVENT);
  await page.getByRole("button", { name: "Retry" }).click();
  await expect(page.getByText("Retry private title")).toBeVisible();
  await expect(
    page.getByText("Couldn’t load reminders. Please try again."),
  ).toHaveCount(0);

  await page.evaluate((event) => {
    const target = window as unknown as {
      __privacyRetryLocked?: boolean;
      __demoEmit: (name: string, payload: unknown) => void;
    };
    target.__privacyRetryLocked = true;
    target.__demoEmit(event, null);
  }, REMINDER_VISIBILITY_EVENT);
  await expect(page.getByText("Retry private title")).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __privacyRetryListCalls?: number;
            }
          ).__privacyRetryListCalls ?? 0,
      ),
    )
    .toBe(2);
  await page.evaluate(
    () =>
      new Promise<void>((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
      }),
  );
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as {
            __privacyRetryListCalls?: number;
          }
        ).__privacyRetryListCalls ?? 0,
    ),
  ).toBe(2);
});

test("Reminders: Retry restores transient live updates without duplicating privacy listeners", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      get_reminder_summary: () => ({ dueInboxCount: 1 }),
      list_reminders: () => {
        const target = window as unknown as {
          __updatesRetryListCalls?: number;
        };
        target.__updatesRetryListCalls =
          (target.__updatesRetryListCalls ?? 0) + 1;
        const now = Date.now();
        return {
          inbox: [
            {
              occurrenceId: "o-updates-retry",
              dueAt: now - 60_000,
              reminder: {
                id: "r-updates-retry",
                title: "Live updates reconnect",
                details: null,
                dueAt: now - 60_000,
                repeatEvery: null,
                repeatUnit: null,
                state: "active",
                origin: "manual",
                createdAt: now - 120_000,
                updatedAt: now - 120_000,
                completedAt: null,
                sources: [],
              },
            },
          ],
          upcoming: [],
          completed: [],
          dueInboxCount: 1,
        };
      },
    },
    {},
    [],
    [],
    {},
    {},
    {
      [REMINDERS_UPDATED_EVENT]: Array.from(
        { length: 20 },
        (_, index) => index + 1,
      ),
    },
  );

  await page.goto("/reminders");
  await expect(page.getByText("Live updates reconnect")).toBeVisible();
  await expect(
    page.getByText(
      "Live reminder updates are unavailable. Retry to reconnect.",
    ),
  ).toBeVisible();
  await page.evaluate((event) => {
    (
      window as unknown as {
        __demoReleaseRejectedEventListeners: (name: string) => void;
      }
    ).__demoReleaseRejectedEventListeners(event);
  }, REMINDERS_UPDATED_EVENT);
  await page.getByRole("button", { name: "Retry" }).click();
  await expect(
    page.getByText(
      "Live reminder updates are unavailable. Retry to reconnect.",
    ),
  ).toHaveCount(0);

  await page.evaluate((event) => {
    (
      window as unknown as {
        __demoEmit: (name: string, payload: unknown) => void;
      }
    ).__demoEmit(event, { dueInboxCount: 2 });
  }, REMINDERS_UPDATED_EVENT);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __updatesRetryListCalls?: number;
            }
          ).__updatesRetryListCalls ?? 0,
      ),
    )
    .toBe(3);
  await page.evaluate(
    () =>
      new Promise<void>((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
      }),
  );
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as {
            __updatesRetryListCalls?: number;
          }
        ).__updatesRetryListCalls ?? 0,
    ),
  ).toBe(3);
});

test("Smart Reminder: visibility-listener registration failure prevents audit and context hydration", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      get_reminder_summary: () => ({ dueInboxCount: 0 }),
      audit_reminder_suggestions: () => {
        const target = window as unknown as {
          __failedVisibilityAuditCalls?: number;
        };
        target.__failedVisibilityAuditCalls =
          (target.__failedVisibilityAuditCalls ?? 0) + 1;
        return [
          {
            id: "sg-listener-failed",
            title: "Must never be audited or rendered",
            suggestedDueAt: null,
            source: {
              kind: "meeting",
              id: "m-atlas-roadmap",
              title: "Must never hydrate context",
            },
          },
        ];
      },
    },
    {},
    [REMINDER_VISIBILITY_EVENT],
  );

  await page.goto("/meeting/m-atlas-roadmap");
  const card = page.locator("app-smart-reminder-card");
  await expect(
    card.getByText(
      "Smart reminder suggestions aren’t available securely right now.",
    ),
  ).toBeVisible();
  await expect(card.getByText("Must never be audited or rendered")).toHaveCount(
    0,
  );
  const newReminder = page
    .getByTestId("meeting-command-bar")
    .getByRole("button", { name: "New reminder" });
  await expect(newReminder).toBeDisabled();
  const composer = page.locator("app-reminder-composer");
  await expect(composer.getByRole("dialog")).toHaveCount(0);
  await expect(
    composer.getByText("Must never hydrate context", { exact: true }),
  ).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as {
            __failedVisibilityAuditCalls?: number;
          }
        ).__failedVisibilityAuditCalls ?? 0,
    ),
  ).toBe(0);
});

test("Reminder composer: a request created before the listener barrier is discarded", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      get_reminder_summary: () => ({ dueInboxCount: 1 }),
      list_reminders: () => {
        const target = window as unknown as {
          __composerBarrierLocked?: boolean;
        };
        const now = Date.now();
        const reminder = {
          id: "r-composer-barrier",
          title: "Source-bearing edit request",
          details: null,
          dueAt: now - 60_000,
          repeatEvery: null,
          repeatUnit: null,
          state: "active",
          origin: "manual",
          createdAt: now - 120_000,
          updatedAt: now - 120_000,
          completedAt: null,
          sources: target.__composerBarrierLocked
            ? []
            : [
                {
                  kind: "meeting",
                  id: "m-composer-barrier",
                  title: "Composer private source title",
                },
              ],
        };
        return {
          inbox: [
            {
              occurrenceId: "o-composer-barrier",
              dueAt: reminder.dueAt,
              reminder,
            },
          ],
          upcoming: [],
          completed: [],
          dueInboxCount: 1,
        };
      },
    },
    {},
    [],
    [],
    { [REMINDER_VISIBILITY_EVENT]: [2] },
  );

  await page.goto("/reminders");
  const reminder = page
    .locator(".reminder-card")
    .filter({ hasText: "Source-bearing edit request" });
  await expect(
    reminder.getByText("Composer private source title"),
  ).toBeVisible();
  const composer = page.locator("app-reminder-composer");

  // The lock event is deliberately emitted before Tauri acknowledges the
  // visibility listener. It is therefore not replayed when registration later
  // succeeds; the pre-barrier request itself must be treated as stale. Native
  // click + event + release run in ONE browser task, so Angular's scheduled
  // effect cannot manufacture a false-green gap between those operations.
  await reminder
    .getByRole("button", { name: "Edit" })
    .evaluate((button, event) => {
      const target = window as unknown as {
        __composerBarrierLocked?: boolean;
        __demoEmit: (name: string, payload: unknown) => void;
        __demoReleaseEventListeners: (name: string) => void;
      };
      (button as HTMLButtonElement).click();
      target.__composerBarrierLocked = true;
      target.__demoEmit(event, null);
      target.__demoReleaseEventListeners(event);
    }, REMINDER_VISIBILITY_EVENT);
  await page.evaluate(
    () =>
      new Promise<void>((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
      }),
  );

  await expect(page.getByText("Composer private source title")).toHaveCount(0);
  await expect(composer.getByRole("dialog")).toHaveCount(0);

  // A request born after the barrier remains usable.
  await page.getByRole("button", { name: "New reminder" }).first().click();
  await expect(composer.getByRole("dialog")).toBeVisible();
  await composer.getByRole("button", { name: "Cancel" }).click();
});

test("Reminder composer: a stale submit cannot close or contaminate a newer request", async ({
  page,
}) => {
  await mockTauri(page, {
    get_reminder_summary: () => ({ dueInboxCount: 0 }),
    list_reminders: () => ({
      inbox: [],
      upcoming: [],
      completed: [],
      dueInboxCount: 0,
    }),
    create_reminder: () => {
      const target = window as unknown as {
        __staleSubmitAttempt?: number;
        __resolveStaleSubmit?: (value: unknown) => void;
        __rejectStaleSubmit?: (error: unknown) => void;
      };
      target.__staleSubmitAttempt = (target.__staleSubmitAttempt ?? 0) + 1;
      return new Promise((resolve, reject) => {
        target.__resolveStaleSubmit = resolve;
        target.__rejectStaleSubmit = reject;
      });
    },
  });

  await page.goto("/reminders");
  const composer = page.locator("app-reminder-composer");
  const openFresh = async (title: string): Promise<void> => {
    await page.getByRole("button", { name: "New reminder" }).first().click();
    await expect(composer.getByRole("dialog")).toBeVisible();
    await composer.locator('input[type="text"]').first().fill(title);
  };
  const emitVisibilityInvalidation = async (): Promise<void> => {
    await page.evaluate((event) => {
      (
        window as unknown as {
          __demoEmit: (name: string, payload: unknown) => void;
        }
      ).__demoEmit(event, null);
    }, REMINDER_VISIBILITY_EVENT);
    await expect(composer.getByRole("dialog")).toHaveCount(0);
  };
  const settleFrames = async (): Promise<void> => {
    await page.evaluate(
      () =>
        new Promise<void>((resolve) => {
          requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
        }),
    );
  };

  await openFresh("Request A");
  await composer.getByRole("button", { name: "Create reminder" }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __staleSubmitAttempt?: number;
            }
          ).__staleSubmitAttempt ?? 0,
      ),
    )
    .toBe(1);
  await emitVisibilityInvalidation();
  await openFresh("Request B survives A");

  await page.evaluate(() => {
    (
      window as unknown as {
        __resolveStaleSubmit?: (value: unknown) => void;
      }
    ).__resolveStaleSubmit?.({
      id: "r-stale-a",
      title: "Request A",
      details: null,
      dueAt: Date.now() + 60_000,
      repeatEvery: null,
      repeatUnit: null,
      state: "active",
      origin: "manual",
      createdAt: Date.now(),
      updatedAt: Date.now(),
      completedAt: null,
      sources: [],
    });
  });
  await settleFrames();
  await expect(composer.getByRole("dialog")).toBeVisible();
  await expect(composer.locator('input[type="text"]').first()).toHaveValue(
    "Request B survives A",
  );
  await expect(
    composer.getByText("Couldn’t save this reminder. Please try again."),
  ).toHaveCount(0);

  await composer.getByRole("button", { name: "Create reminder" }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __staleSubmitAttempt?: number;
            }
          ).__staleSubmitAttempt ?? 0,
      ),
    )
    .toBe(2);
  await emitVisibilityInvalidation();
  await openFresh("Request C survives B");
  await page.evaluate(() => {
    (
      window as unknown as {
        __rejectStaleSubmit?: (error: unknown) => void;
      }
    ).__rejectStaleSubmit?.(new Error("old request failed"));
  });
  await settleFrames();

  await expect(composer.getByRole("dialog")).toBeVisible();
  await expect(composer.locator('input[type="text"]').first()).toHaveValue(
    "Request C survives B",
  );
  await expect(
    composer.getByText("Couldn’t save this reminder. Please try again."),
  ).toHaveCount(0);
  await expect(
    composer.getByRole("button", { name: "Create reminder" }),
  ).toBeEnabled();
  await composer.getByRole("button", { name: "Cancel" }).click();
});

test("Reminder composer: focus, source limit, and busy source locking stay coherent", async ({
  page,
}) => {
  await mockTauri(page, {
    get_reminder_summary: () => ({ dueInboxCount: 1 }),
    list_link_candidates: () => [
      {
        kind: "note",
        id: "n20",
        title: "Source 20",
        snippet: "",
        createdAt: "",
      },
    ],
    list_reminders: () => {
      const now = Date.now();
      const reminder = {
        id: "r-source-limit",
        title: "Twenty-source reminder",
        details: null,
        dueAt: now - 60_000,
        repeatEvery: null,
        repeatUnit: null,
        state: "active",
        origin: "manual",
        createdAt: now - 120_000,
        updatedAt: now - 120_000,
        completedAt: null,
        sources: Array.from({ length: 20 }, (_, index) => ({
          kind: "note",
          id: `n${index}`,
          title: `Source ${index}`,
        })),
      };
      return {
        inbox: [
          {
            occurrenceId: "o-source-limit",
            dueAt: reminder.dueAt,
            reminder,
          },
        ],
        upcoming: [],
        completed: [],
        dueInboxCount: 1,
      };
    },
    update_reminder: () =>
      new Promise((resolve) => {
        (
          window as unknown as {
            __resolveBusyUpdate?: (value: unknown) => void;
          }
        ).__resolveBusyUpdate = resolve;
      }),
  });

  await page.goto("/reminders");
  const row = page
    .locator(".reminder-card")
    .filter({ hasText: "Twenty-source reminder" });
  const edit = row.getByRole("button", { name: "Edit" });
  const composer = page.locator("app-reminder-composer");

  await edit.click();
  const dialog = composer.getByRole("dialog");
  const titleInput = composer.locator('input[type="text"]').first();
  await expect(titleInput).toBeFocused();
  const close = dialog.getByRole("button", {
    name: "Close reminder composer",
  });
  const save = dialog.getByRole("button", { name: "Save changes" });
  await close.focus();
  await page.keyboard.press("Shift+Tab");
  await expect(save).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(edit).toBeFocused();

  await edit.click();
  const addSource = dialog.getByRole("button", { name: "+ Add source" });
  await expect(addSource).toBeDisabled();
  const removeSource0 = dialog.getByRole("button", {
    name: "Remove Source 0",
    exact: true,
  });
  await expect(removeSource0).toBeEnabled();
  await removeSource0.click();
  await expect(addSource).toBeEnabled();
  await addSource.click();
  await page.getByRole("option", { name: /Source 20/ }).click();
  await expect(addSource).toBeDisabled();
  await expect(dialog.getByText("20 / 20")).toBeVisible();
  await dialog.getByRole("button", { name: "+17 more" }).click();
  await expect(
    dialog.getByRole("button", { name: "Remove Source 20", exact: true }),
  ).toBeEnabled();

  await dialog
    .getByRole("button", { name: "Remove Source 1", exact: true })
    .click();
  await expect(addSource).toBeEnabled();
  await save.click();
  await expect(addSource).toBeDisabled();
  await expect(
    dialog.getByRole("button", { name: "Remove Source 2", exact: true }),
  ).toBeDisabled();

  await page.evaluate(() => {
    (
      window as unknown as {
        __resolveBusyUpdate?: (value: unknown) => void;
      }
    ).__resolveBusyUpdate?.({
      id: "r-source-limit",
      title: "Twenty-source reminder",
      details: null,
      dueAt: Date.now() + 60_000,
      repeatEvery: null,
      repeatUnit: null,
      state: "active",
      origin: "manual",
      createdAt: Date.now(),
      updatedAt: Date.now(),
      completedAt: null,
      sources: [],
    });
  });
  await expect(dialog).toHaveCount(0);
});

test("Smart Reminder: a lock before listener readiness cannot rehydrate the parent title", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      get_reminder_summary: () => ({ dueInboxCount: 0 }),
      audit_reminder_suggestions: () => {
        const target = window as unknown as {
          __listenerBarrierLocked?: boolean;
          __listenerBarrierAuditCalls?: number;
        };
        target.__listenerBarrierAuditCalls =
          (target.__listenerBarrierAuditCalls ?? 0) + 1;
        if (target.__listenerBarrierLocked) {
          return Promise.reject(new Error("locked: source is sealed"));
        }
        return [
          {
            id: "sg-listener-barrier",
            title: "Unsafe pre-lock suggestion",
            suggestedDueAt: null,
            source: {
              kind: "meeting",
              id: "m-atlas-roadmap",
              title: "Q2 Roadmap Planning",
            },
          },
        ];
      },
    },
    {},
    [],
    [REMINDER_VISIBILITY_EVENT],
  );

  await page.goto("/meeting/m-atlas-roadmap");
  await page.evaluate(() => {
    const target = window as unknown as {
      __listenerBarrierLocked?: boolean;
      __TAURI_INTERNALS__: {
        invoke: (command: string, args: unknown) => Promise<unknown>;
      };
    };
    const invoke = target.__TAURI_INTERNALS__.invoke.bind(
      target.__TAURI_INTERNALS__,
    );
    target.__TAURI_INTERNALS__.invoke = (command, args) =>
      command === "get_meeting_detail" && target.__listenerBarrierLocked
        ? Promise.resolve(null)
        : invoke(command, args);
  });
  const card = page.locator("app-smart-reminder-card");
  const newReminder = page
    .getByTestId("meeting-command-bar")
    .getByRole("button", { name: "New reminder" });
  await expect(newReminder).toBeDisabled();
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as {
            __listenerBarrierAuditCalls?: number;
          }
        ).__listenerBarrierAuditCalls ?? 0,
    ),
  ).toBe(0);

  await page.evaluate((event) => {
    const target = window as unknown as {
      __listenerBarrierLocked?: boolean;
      __demoEmit: (name: string, payload: unknown) => void;
      __demoReleaseEventListeners: (name: string) => void;
    };
    target.__listenerBarrierLocked = true;
    target.__demoEmit(event, null);
    target.__demoReleaseEventListeners(event);
  }, REMINDER_VISIBILITY_EVENT);

  await expect(newReminder).toBeEnabled();
  await expect(
    card.getByText("Smart reminder suggestions aren’t available right now."),
  ).toBeVisible();
  await expect(card.getByText("Unsafe pre-lock suggestion")).toHaveCount(0);

  await newReminder.click();
  const composer = page.locator("app-reminder-composer");
  await expect(composer.getByRole("dialog")).toBeVisible();
  await expect(
    composer.getByText("Q2 Roadmap Planning", { exact: true }),
  ).toHaveCount(0);
  await composer.getByRole("button", { name: "Cancel" }).click();
});

test("Smart Reminder: a mounted meeting card drops stale rows after every canonical source edit", async ({
  page,
}) => {
  test.setTimeout(30_000);
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") {
      errors.push(message.text());
    }
  });
  page.on("pageerror", (error) => errors.push(String(error)));

  await mockTauri(page, {
    get_reminder_summary: () => ({ dueInboxCount: 0 }),
    audit_reminder_suggestions: (args: {
      sourceKind: "meeting" | "note";
      sourceId: string;
    }) => {
      const target = window as unknown as {
        __smartSourceState?: {
          phase: "before" | "title" | "transcript" | "locked";
          auditCalls: number;
          events: Array<Record<string, unknown>>;
        };
      };
      target.__smartSourceState ??= {
        phase: "before",
        auditCalls: 0,
        events: [],
      };
      target.__smartSourceState.auditCalls += 1;
      if (target.__smartSourceState.phase === "locked") {
        return Promise.reject(new Error("locked: source is sealed"));
      }
      const titles = {
        before: "Before source edit",
        title: "After title edit",
        transcript: "After transcript or manual-note edit",
      };
      return [
        {
          id: `sg-${target.__smartSourceState.phase}`,
          title: titles[target.__smartSourceState.phase],
          suggestedDueAt: null,
          source: {
            kind: args.sourceKind,
            id: args.sourceId,
            title: "Q2 Roadmap Planning",
          },
        },
      ];
    },
    rename_meeting: () => {
      const target = window as unknown as {
        __smartSourceState: {
          phase: "before" | "title" | "transcript" | "locked";
          auditCalls: number;
          events: Array<Record<string, unknown>>;
        };
        __demoEmit: (event: string, payload: unknown) => void;
      };
      target.__smartSourceState.phase = "title";
      const payload = { kind: "meeting", id: "m-atlas-roadmap" };
      target.__smartSourceState.events.push(payload);
      target.__demoEmit("murmur://reminder-source-updated", payload);
      return null;
    },
  });

  await page.goto("/meeting/m-atlas-roadmap");
  await page.evaluate(() => {
    const target = window as unknown as {
      __smartSourceState?: { phase: string };
      __TAURI_INTERNALS__: {
        invoke: (command: string, args: unknown) => Promise<unknown>;
      };
    };
    const invoke = target.__TAURI_INTERNALS__.invoke.bind(
      target.__TAURI_INTERNALS__,
    );
    target.__TAURI_INTERNALS__.invoke = (command, args) =>
      command === "get_meeting_detail" &&
        target.__smartSourceState?.phase === "locked"
        ? Promise.resolve(null)
        : invoke(command, args);
  });
  const card = page.locator("app-smart-reminder-card");
  await expect(card.getByText("Before source edit")).toBeVisible();
  const initialAudits = await page.evaluate(
    () =>
      (
        window as unknown as {
          __smartSourceState: { auditCalls: number };
        }
      ).__smartSourceState.auditCalls,
  );

  // The real title-edit UI and the opaque backend event can race; both paths
  // synchronously clear and coalesce into one debounced re-audit.
  const commandBar = page.getByTestId("meeting-command-bar");
  await commandBar.getByRole("button", { name: /More/ }).click();
  await commandBar.getByRole("menuitem", { name: "Rename" }).click();
  await page.getByLabel("Meeting title").fill("Renamed roadmap");
  await page.locator(".rename").getByRole("button", { name: "Save" }).click();
  await expect(card.getByText("Before source edit")).toHaveCount(0, {
    timeout: 700,
  });
  await expect(card.getByText("After title edit")).toBeVisible();
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            (
              window as unknown as {
                __smartSourceState: { auditCalls: number };
              }
            ).__smartSourceState.auditCalls,
        ),
      { timeout: 5_000 },
    )
    .toBe(initialAudits + 1);

  // A source-specific event must not cross-bleed into another mounted card.
  await page.evaluate(() => {
    const target = window as unknown as {
      __smartSourceState: {
        events: Array<Record<string, unknown>>;
      };
      __demoEmit: (event: string, payload: unknown) => void;
    };
    const payload = { kind: "meeting", id: "m-other" };
    target.__smartSourceState.events.push(payload);
    target.__demoEmit("murmur://reminder-source-updated", payload);
  });
  await page.waitForTimeout(1_000);
  await expect(card.getByText("After title edit")).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as {
            __smartSourceState: { auditCalls: number };
          }
        ).__smartSourceState.auditCalls,
    ),
  ).toBe(initialAudits + 1);

  // Simulate a canonical transcript/manual-notes write that changes no current
  // Angular input. Only the content-free source event can invalidate this row.
  await page.evaluate(() => {
    const target = window as unknown as {
      __smartSourceState: {
        phase: "before" | "title" | "transcript" | "locked";
        events: Array<Record<string, unknown>>;
      };
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__smartSourceState.phase = "transcript";
    const payload = { kind: "meeting", id: "m-atlas-roadmap" };
    target.__smartSourceState.events.push(payload);
    target.__demoEmit("murmur://reminder-source-updated", payload);
  });
  await expect(card.getByText("After title edit")).toHaveCount(0, {
    timeout: 700,
  });
  await expect(
    card.getByText("After transcript or manual-note edit"),
  ).toBeVisible();
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            (
              window as unknown as {
                __smartSourceState: { auditCalls: number };
              }
            ).__smartSourceState.auditCalls,
        ),
      { timeout: 5_000 },
    )
    .toBe(initialAudits + 2);

  // Global lock-authority revocation carries no source identity. Every mounted
  // card must drop derived text immediately, then let the gated audit fail
  // closed while the source remains sealed.
  await page.evaluate(() => {
    const target = window as unknown as {
      __smartSourceState: {
        phase: "before" | "title" | "transcript" | "locked";
      };
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__smartSourceState.phase = "locked";
    target.__demoEmit("murmur://reminder-visibility-invalidated", null);
  });
  await expect(
    card.getByText("After transcript or manual-note edit"),
  ).toHaveCount(0, { timeout: 700 });
  await commandBar.getByRole("button", { name: "New reminder" }).click();
  const composer = page.locator("app-reminder-composer");
  await expect(composer.getByRole("dialog")).toBeVisible();
  await expect(
    composer.getByText("Q2 Roadmap Planning", { exact: true }),
  ).toHaveCount(0);
  await composer.getByRole("button", { name: "Cancel" }).click();
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            (
              window as unknown as {
                __smartSourceState: { auditCalls: number };
              }
            ).__smartSourceState.auditCalls,
        ),
      { timeout: 5_000 },
    )
    .toBe(initialAudits + 3);
  await expect(
    card.getByText("Smart reminder suggestions aren’t available right now."),
  ).toBeVisible();

  // A later count-only event changes the root store revision. The still-mounted
  // card must not copy its stale parent title back after the visibility barrier.
  await page.evaluate(() => {
    const target = window as unknown as {
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__demoEmit("murmur://reminders-updated", { dueInboxCount: 0 });
  });
  await commandBar.getByRole("button", { name: "New reminder" }).click();
  await expect(composer.getByRole("dialog")).toBeVisible();
  await expect(
    composer.getByText("Q2 Roadmap Planning", { exact: true }),
  ).toHaveCount(0);
  await composer.getByRole("button", { name: "Cancel" }).click();

  const eventPayloads = await page.evaluate(
    () =>
      (
        window as unknown as {
          __smartSourceState: {
            events: Array<Record<string, unknown>>;
          };
        }
      ).__smartSourceState.events,
  );
  expect(eventPayloads).toHaveLength(3);
  for (const payload of eventPayloads) {
    expect(Object.keys(payload).sort()).toEqual(["id", "kind"]);
  }
  expect(errors).toEqual([]);
});

test("Reminders: route, composer, inbox, Smart review, context, and event refresh", async ({
  page,
}) => {
  test.setTimeout(90_000);
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") {
      errors.push(message.text());
    }
  });
  page.on("pageerror", (error) => errors.push(String(error)));

  // One Tauri setup before any navigation. Every override is page-side and
  // self-contained because mockTauri serializes functions with toString().
  await mockTauri(page, {
    get_reminder_summary: () => ({ dueInboxCount: 2 }),
    list_folders: () => {
      const target = window as unknown as {
        __reminderLockState?: { locked: boolean };
      };
      target.__reminderLockState ??= { locked: false };
      return [
        {
          id: "nf-product",
          name: "Product Notes",
          path: "Notes/Product",
          parentId: null,
          noteCount: 1,
          locked: true,
          unlocked: !target.__reminderLockState.locked,
          kind: "note",
          children: [],
        },
      ];
    },
    relock_all: () => {
      const target = window as unknown as {
        __reminderLockState?: { locked: boolean };
      };
      target.__reminderLockState ??= { locked: false };
      target.__reminderLockState.locked = true;
      return null;
    },
    get_note: (args: { id: string }) => {
      const target = window as unknown as {
        __reminderLockState?: { locked: boolean };
      };
      target.__reminderLockState ??= { locked: false };
      if (target.__reminderLockState.locked) {
        return {
          id: args.id,
          title: "🔒 Locked",
          folderId: "nf-product",
          markdown: "",
          tags: [],
          properties: {},
          updatedAt: Date.now(),
          createdAt: Date.now() - 86400000,
          exportedPath: null,
          locked: true,
          shared: false,
        };
      }
      return {
        id: args.id,
        title: "Atlas — PRD v3",
        folderId: "nf-product",
        markdown:
          "# Atlas — PRD v3\n\n## Open questions\n- Confirm the launch owner.",
        tags: ["atlas", "prd"],
        properties: { status: "in-review" },
        updatedAt: Date.now() - 3600000,
        createdAt: Date.now() - 86400000,
        exportedPath: "/Vault/Notes/Product/Atlas-PRD-v3.md",
        locked: false,
        shared: false,
      };
    },
    list_reminders: () => {
      const target = window as unknown as {
        __reminderState?: Record<string, any>;
      };
      target.__reminderState ??= {
        listCalls: 0,
        inbox: [
          {
            occurrenceId: "o-1",
            dueAt: Date.now() - 7200000,
            reminder: {
              id: "r-due-1",
              title: "Send the roadmap follow-up",
              details: "Include the final rollout date.",
              dueAt: Date.now() - 7200000,
              repeatEvery: null,
              repeatUnit: null,
              state: "active",
              origin: "manual",
              createdAt: Date.now() - 9000000,
              updatedAt: Date.now() - 9000000,
              completedAt: null,
              sources: [],
            },
          },
          {
            occurrenceId: "o-2",
            dueAt: Date.now() - 3600000,
            reminder: {
              id: "r-due-2",
              title: "Book the pilot review",
              details: null,
              dueAt: Date.now() - 3600000,
              repeatEvery: 1,
              repeatUnit: "weeks",
              state: "active",
              origin: "manual",
              createdAt: Date.now() - 8000000,
              updatedAt: Date.now() - 8000000,
              completedAt: null,
              sources: [],
            },
          },
        ],
        upcoming: [
          {
            id: "r-smart",
            title: "Confirm the Atlas launch owner",
            details: null,
            dueAt: Date.now() + 86400000,
            repeatEvery: null,
            repeatUnit: null,
            state: "active",
            origin: "smart",
            createdAt: Date.now(),
            updatedAt: Date.now(),
            completedAt: null,
            sources: [
              {
                kind: "meeting",
                id: "m-atlas-roadmap",
                title: "Q2 Roadmap Planning",
              },
            ],
          },
        ],
        completed: [],
        suggestions: {
          meeting: [
            {
              id: "sg-meeting-1",
              title: "Send the reviewed roadmap",
              suggestedDueAt: Date.now() + 10800000,
              source: {
                kind: "meeting",
                id: "m-atlas-roadmap",
                title: "Q2 Roadmap Planning",
              },
            },
            {
              id: "sg-meeting-2",
              title: "Schedule the pilot follow-up",
              suggestedDueAt: null,
              source: {
                kind: "meeting",
                id: "m-atlas-roadmap",
                title: "Q2 Roadmap Planning",
              },
            },
          ],
          note: [
            {
              id: "sg-note-1",
              title: "Resolve the PRD open question",
              suggestedDueAt: null,
              source: {
                kind: "note",
                id: "n-atlas-prd",
                title: "Atlas — PRD v3",
              },
            },
          ],
        },
        calls: [],
      };
      target.__reminderState.listCalls += 1;
      return {
        inbox: target.__reminderState.inbox,
        upcoming: target.__reminderState.upcoming,
        completed: target.__reminderState.completed,
        dueInboxCount: target.__reminderState.inbox.length,
      };
    },
    create_reminder: (args: { draft: Record<string, unknown> }) => {
      const target = window as unknown as {
        __reminderState: Record<string, any>;
      };
      target.__reminderState.calls.push({ command: "create", args });
      const reminder = {
        id: "r-created",
        ...args.draft,
        state: "active",
        origin: "manual",
        createdAt: Date.now(),
        updatedAt: Date.now(),
        completedAt: null,
        sources: (
          args.draft.sources as Array<{ kind: string; id: string }>
        ).map((source) => ({
          ...source,
          title:
            source.kind === "meeting" ? "Atlas meeting" : "Atlas project note",
        })),
      };
      target.__reminderState.upcoming.push(reminder);
      return reminder;
    },
    complete_reminder: (args: {
      reminderId: string;
      expectedDueAt: number;
    }) => {
      const target = window as unknown as {
        __reminderState: Record<string, any>;
      };
      target.__reminderState.calls.push({ command: "complete", args });
      const item = target.__reminderState.inbox.find(
        (row: any) => row.reminder.id === args.reminderId,
      );
      target.__reminderState.inbox = target.__reminderState.inbox.filter(
        (row: any) => row.reminder.id !== args.reminderId,
      );
      if (item) {
        if (
          item.reminder.repeatEvery === 1 &&
          item.reminder.repeatUnit === "weeks"
        ) {
          target.__reminderState.upcoming.push({
            ...item.reminder,
            dueAt: args.expectedDueAt + 7 * 24 * 60 * 60 * 1000,
            updatedAt: Date.now(),
          });
        } else {
          target.__reminderState.completed.push({
            ...item.reminder,
            state: "completed",
            completedAt: Date.now(),
          });
        }
      }
      return null;
    },
    dismiss_reminder_occurrence: (args: { occurrenceId: string }) => {
      const target = window as unknown as {
        __reminderState: Record<string, any>;
      };
      target.__reminderState.calls.push({ command: "dismissOccurrence", args });
      const item = target.__reminderState.inbox.find(
        (row: any) => row.occurrenceId === args.occurrenceId,
      );
      target.__reminderState.inbox = target.__reminderState.inbox.filter(
        (row: any) => row.occurrenceId !== args.occurrenceId,
      );
      if (item) {
        target.__reminderState.upcoming.push({
          ...item.reminder,
          dueAt:
            item.reminder.repeatEvery === 1 &&
            item.reminder.repeatUnit === "weeks"
              ? item.dueAt + 7 * 24 * 60 * 60 * 1000
              : item.reminder.dueAt,
          updatedAt: Date.now(),
        });
      }
      return null;
    },
    audit_reminder_suggestions: (args: {
      sourceKind: "meeting" | "note";
      sourceId: string;
    }) => {
      const target = window as unknown as {
        __reminderState: Record<string, any>;
      };
      target.__reminderState.calls.push({ command: "audit", args });
      return target.__reminderState.suggestions[args.sourceKind] ?? [];
    },
    dismiss_reminder_suggestion: (args: { suggestionId: string }) => {
      const target = window as unknown as {
        __reminderState: Record<string, any>;
      };
      target.__reminderState.calls.push({ command: "dismissSuggestion", args });
      for (const kind of ["meeting", "note"]) {
        target.__reminderState.suggestions[kind] =
          target.__reminderState.suggestions[kind].filter(
            (row: any) => row.id !== args.suggestionId,
          );
      }
      return null;
    },
    accept_reminder_suggestion: (args: {
      suggestionId: string;
      draft: Record<string, unknown>;
    }) => {
      const target = window as unknown as {
        __reminderState: Record<string, any>;
      };
      target.__reminderState.calls.push({ command: "acceptSuggestion", args });
      for (const kind of ["meeting", "note"]) {
        target.__reminderState.suggestions[kind] =
          target.__reminderState.suggestions[kind].filter(
            (row: any) => row.id !== args.suggestionId,
          );
      }
      const reminder = {
        id: "r-accepted",
        ...args.draft,
        state: "active",
        origin: "smart",
        createdAt: Date.now(),
        updatedAt: Date.now(),
        completedAt: null,
        sources: [],
      };
      target.__reminderState.upcoming.push(reminder);
      return reminder;
    },
    list_link_candidates: () => [
      {
        kind: "meeting",
        id: "m-source",
        title: "Atlas meeting",
        snippet: "",
      },
      {
        kind: "note",
        id: "n-source",
        title: "Atlas project note",
        snippet: "",
      },
      {
        kind: "document",
        id: "d-hidden",
        title: "Generic imported PDF",
        snippet: "",
      },
    ],
  });

  await page.goto("/reminders");

  const reminderNav = (await openBrowse(page)).getByRole("link", {
    name: "Reminders",
  });
  await expect(reminderNav).toBeVisible();
  await expect(reminderNav.locator(".count")).toHaveText("2");
  await expect(page.getByRole("heading", { name: "Reminders" })).toBeVisible();
  await expect(page.getByText("Send the roadmap follow-up")).toBeVisible();

  // Composer: recurrence + two visible sources, never the generic document.
  await page.getByRole("button", { name: "New reminder" }).first().click();
  const composer = page.locator("app-reminder-composer");
  await composer.getByLabel("Title").fill("Prepare launch review");
  await composer.getByLabel("Details optional").fill("Bring the owner matrix.");
  await composer.getByLabel("Date").fill("2026-08-15");
  await composer.getByLabel("Time").fill("09:30");
  await composer.getByLabel("Repeat").check();
  await composer.getByLabel("Repeat every").fill("2");
  await composer.getByLabel("Repeat unit").selectOption("weeks");
  await composer.getByRole("button", { name: "+ Add source" }).click();
  await expect(page.getByText("Generic imported PDF")).toHaveCount(0);
  await page.getByRole("option", { name: /Atlas meeting/ }).click();
  await page.getByRole("option", { name: /Atlas project note/ }).click();
  await page.locator(".sp-search").press("Escape");
  await composer.getByRole("button", { name: "Create reminder" }).click();
  await expect(composer.getByRole("dialog")).toHaveCount(0);

  const createCall = await page.evaluate(() => {
    const state = (
      window as unknown as { __reminderState: Record<string, any> }
    ).__reminderState;
    return state.calls.find((call: any) => call.command === "create");
  });
  expect(createCall.args.draft).toMatchObject({
    title: "Prepare launch review",
    details: "Bring the owner matrix.",
    repeatEvery: 2,
    repeatUnit: "weeks",
    sources: [
      { kind: "meeting", id: "m-source" },
      { kind: "note", id: "n-source" },
    ],
  });
  expect(createCall.args.draft.sources[0]).not.toHaveProperty("title");

  // Inbox confirm-then-refresh actions.
  await page
    .locator(".reminder-card")
    .filter({ hasText: "Book the pilot review" })
    .getByRole("button", { name: "Complete" })
    .click();
  await expect(page.getByText("Book the pilot review")).toHaveCount(0);
  await page
    .locator(".reminder-card")
    .filter({ hasText: "Send the roadmap follow-up" })
    .getByRole("button", { name: "Dismiss" })
    .click();
  await expect(page.getByText("Send the roadmap follow-up")).toHaveCount(0);

  await page.getByRole("button", { name: "Upcoming" }).click();
  const recurringUpcoming = page
    .locator(".reminder-card")
    .filter({ hasText: "Book the pilot review" });
  const dismissedOneOff = page
    .locator(".reminder-card")
    .filter({ hasText: "Send the roadmap follow-up" });
  await expect(recurringUpcoming).toBeVisible();
  await expect(dismissedOneOff).toBeVisible();
  await expect(recurringUpcoming.getByText("↻ Every week")).toBeVisible();
  const nextRecurringLabel = await page.evaluate(() => {
    const reminder = (
      window as unknown as { __reminderState: Record<string, any> }
    ).__reminderState.upcoming.find((row: any) => row.id === "r-due-2");
    return new Intl.DateTimeFormat(undefined, {
      weekday: "short",
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    }).format(new Date(reminder.dueAt));
  });
  await expect(
    recurringUpcoming.locator(".reminder-meta span").first(),
  ).toHaveText(nextRecurringLabel);
  expect(
    await page.evaluate(() => {
      const reminder = (
        window as unknown as { __reminderState: Record<string, any> }
      ).__reminderState.upcoming.find((row: any) => row.id === "r-due-1");
      return {
        state: reminder.state,
        remainsPastDue: reminder.dueAt < Date.now(),
      };
    }),
  ).toEqual({ state: "active", remainsPastDue: true });
  await expect(page.getByText("✦ Smart").first()).toBeVisible();
  await page.getByRole("button", { name: "Completed" }).click();
  await expect(page.getByText("Book the pilot review")).toHaveCount(0);
  await expect(page.getByText("Send the roadmap follow-up")).toHaveCount(0);
  await expect(page.getByText("No completed reminders")).toBeVisible();
  await page.getByRole("button", { name: "Upcoming" }).click();

  // Count-only event updates both nav badge and canonical rows by refetch.
  const callsBeforeEvent = await page.evaluate(
    () =>
      (window as unknown as { __reminderState: Record<string, any> })
        .__reminderState.listCalls,
  );
  await page.evaluate(() => {
    const target = window as unknown as {
      __reminderState: Record<string, any>;
      __demoEmit: (event: string, payload: unknown) => void;
    };
    target.__reminderState.inbox = [0, 1, 2, 3].map((index) => ({
      occurrenceId: `o-event-${index}`,
      dueAt: Date.now() - index * 60000,
      reminder: {
        id: `r-event-${index}`,
        title: `Event reminder ${index + 1}`,
        details: null,
        dueAt: Date.now() - index * 60000,
        repeatEvery: null,
        repeatUnit: null,
        state: "active",
        origin: "manual",
        createdAt: Date.now(),
        updatedAt: Date.now(),
        completedAt: null,
        sources: [],
      },
    }));
    target.__demoEmit("murmur://reminders-updated", { dueInboxCount: 4 });
  });
  await expect(reminderNav.locator(".count")).toHaveText("4");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __reminderState: Record<string, any> })
            .__reminderState.listCalls,
      ),
    )
    .toBeGreaterThan(callsBeforeEvent);

  // Meeting context: manual source prefill, Escape in-dialog, explicit
  // suggestion dismiss, then edit-before-accept with Smart provenance.
  await page
    .locator(".reminder-card")
    .filter({ hasText: "Confirm the Atlas launch owner" })
    .getByRole("button", { name: /Meeting · Q2 Roadmap Planning/ })
    .click();
  const meetingCard = page.locator("app-smart-reminder-card");
  await expect(meetingCard).toBeVisible();
  await page
    .getByTestId("meeting-command-bar")
    .getByRole("button", { name: "New reminder" })
    .click();
  await expect(composer.getByText("Q2 Roadmap Planning")).toBeVisible();
  await composer.getByLabel("Title").press("Escape");
  await expect(composer.getByRole("dialog")).toHaveCount(0);

  await meetingCard
    .locator(".suggestion-row")
    .filter({ hasText: "Send the reviewed roadmap" })
    .getByRole("button", { name: "Dismiss" })
    .click();
  await expect(meetingCard.getByText("Send the reviewed roadmap")).toHaveCount(
    0,
  );

  await meetingCard
    .locator(".suggestion-row")
    .filter({ hasText: "Schedule the pilot follow-up" })
    .getByRole("button", { name: "Edit & create" })
    .click();
  await expect(composer.getByLabel("Date")).toHaveValue("");
  await expect(composer.getByLabel("Time")).toHaveValue("");
  await composer.getByLabel("Date").fill("2026-08-20");
  await composer.getByLabel("Time").fill("14:00");
  await composer.getByRole("button", { name: "Create reminder" }).click();
  await expect(meetingCard.locator(".suggestion-row")).toHaveCount(0);

  const smartCalls = await page.evaluate(
    () =>
      (window as unknown as { __reminderState: Record<string, any> })
        .__reminderState.calls,
  );
  expect(
    smartCalls.some(
      (call: any) =>
        call.command === "dismissSuggestion" &&
        call.args.suggestionId === "sg-meeting-1",
    ),
  ).toBe(true);
  expect(
    smartCalls.some(
      (call: any) =>
        call.command === "acceptSuggestion" &&
        call.args.suggestionId === "sg-meeting-2",
    ),
  ).toBe(true);

  // Routed authored-note context carries the note source into the same composer.
  await (await openBrowse(page))
    .getByRole("link", { name: "Notes", exact: true })
    .click();
  await page
    .getByRole("button", { name: /Atlas — PRD v3/ })
    .first()
    .click();
  const noteCard = page.locator("app-smart-reminder-card");
  await expect(noteCard).toBeVisible();
  await noteCard.getByRole("button", { name: "New reminder" }).click();
  await expect(composer.getByText("Atlas — PRD v3")).toBeVisible();
  await composer.getByRole("button", { name: "Cancel" }).click();

  // A committed authored-note edit updates sourceRevision and re-audits once
  // after the debounce, instead of once per keystroke/autosave frame.
  const noteAuditsBeforeEdit = await page.evaluate(
    () =>
      (
        window as unknown as { __reminderState: Record<string, any> }
      ).__reminderState.calls.filter(
        (call: any) =>
          call.command === "audit" && call.args.sourceKind === "note",
      ).length,
  );
  // "Atlas — PRD v3" carries a body, so the routed editor opens in Preview now.
  await enterEditMode(page);
  const noteBody = page.locator(".body-area");
  await noteBody.evaluate((element) => {
    const textarea = element as HTMLTextAreaElement;
    for (const value of [
      "## Open questions\n\nConfirm the launch owner.",
      "## Open questions\n\nConfirm the launch owner and legal review draft.",
      "## Open questions\n\nConfirm the launch owner and legal review.",
    ]) {
      textarea.value = value;
      textarea.dispatchEvent(new InputEvent("input", { bubbles: true }));
    }
  });
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            (
              window as unknown as { __reminderState: Record<string, any> }
            ).__reminderState.calls.filter(
              (call: any) =>
                call.command === "audit" && call.args.sourceKind === "note",
            ).length,
        ),
      { timeout: 5_000 },
    )
    .toBe(noteAuditsBeforeEdit + 1);
  await page.waitForTimeout(1_200);
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as { __reminderState: Record<string, any> }
        ).__reminderState.calls.filter(
          (call: any) =>
            call.command === "audit" && call.args.sourceKind === "note",
        ).length,
    ),
  ).toBe(noteAuditsBeforeEdit + 1);

  // Relock publishes the folder tree first; the editor synchronously masks the
  // note and destroys the Smart card, so staged suggestion plaintext cannot
  // survive the lock transition.
  await expect(
    noteCard.getByText("Resolve the PRD open question"),
  ).toBeVisible();
  await page
    .getByRole("button", { name: /Re-seal all 1 unlocked folders? now/ })
    .click();
  await expect(page.getByText("Resolve the PRD open question")).toHaveCount(0);
  await expect(noteCard).toHaveCount(0);

  expect(errors).toEqual([]);
});
