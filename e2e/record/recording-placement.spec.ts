import { expect, test, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

const container = (
  id: string,
  name: string,
  level: "project" | "folder",
  locked: boolean,
  unlocked: boolean,
  folders: unknown[] = [],
) => ({
  id,
  name,
  kind: "meeting",
  level,
  emoji: null,
  tint: null,
  locked,
  unlocked,
  isRoot: false,
  folders,
  groups: [],
});

const FOREST = [
  container("p-acme", "Acme", "project", false, false, [
    container("f-weekly", "Weekly", "folder", false, false),
    {
      ...container("notes-root", "Notes", "folder", false, false, [
        {
          ...container("f-ideas", "Ideas", "folder", false, false),
          kind: "note",
        },
      ]),
      kind: "note",
      isRoot: true,
    },
    container(
      "f-session-locked",
      "Unlocked for viewing",
      "folder",
      true,
      true,
      [container("f-below-lock", "Private child", "folder", false, false)],
    ),
    container("f-sealed", "Sealed", "folder", true, false, [
      container("f-hidden", "Must not render", "folder", false, false),
    ]),
  ]),
];

async function openRecord(
  page: Page,
  overrides: Record<string, (args: any) => unknown> = {},
): Promise<void> {
  await mockTauri(
    page,
    {
      start_recording: (args: unknown) => {
        const target = window as unknown as { __starts?: unknown[] };
        (target.__starts ??= []).push(args);
        return { meetingId: "m-rec", startedAt: "2026-08-27T09:00:00Z" };
      },
      stop_recording: () => ({
        meetingId: "m-rec",
        markdown: "# Notes",
        exportedPath: "/vault/Notes/m-rec.md",
      }),
      get_last_note: () => ({
        meetingId: "m-rec",
        providerId: "claude_code",
        markdown: "# Notes",
        exportedPath: "/vault/Notes/m-rec.md",
      }),
      get_meeting_detail: () => ({
        locked: false,
        meeting: {
          id: "m-rec",
          startedAt: "2026-08-27T09:00:00Z",
          endedAt: "2026-08-27T09:01:00Z",
          title: "Notes",
          durationS: 60,
          audioPath: null,
          status: "EXPORTED",
          folderId:
            (window as unknown as { __recordFolderId?: string })
              .__recordFolderId ?? null,
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      move_note: (args: unknown) => {
        const target = window as unknown as {
          __moves?: unknown[];
          __recordFolderId?: string;
        };
        (target.__moves ??= []).push(args);
        target.__recordFolderId = (args as { folderId: string }).folderId;
        return null;
      },
      ...overrides,
    },
    {
      model_present: true,
      recording_status: { recording: false, meetingId: null, startedAt: null },
      list_workspace_tree: FOREST,
    },
  );
  await page.goto("/record");
  await expect(page.locator("button.start-btn")).toBeVisible();
}

async function finishRecording(page: Page): Promise<void> {
  await page.locator("button.start-btn").click();
  await expect(page.locator("button.stop-btn")).toBeVisible();
  await page.locator("button.stop-btn").click();
  await expect(page.getByTestId("recording-result")).toBeVisible();
}

/**
 * Leave the recorder and come back.
 *
 * Settings is a MODAL: while it is open the shell behind it is under a scrim,
 * so the way back is closing the dialog — which returns to the route we came
 * from — not clicking a sidebar link the scrim intercepts.
 */
async function navigateAwayAndBack(page: Page): Promise<void> {
  await page.getByRole("link", { name: "Settings" }).click();
  await expect(page).toHaveURL(/\/settings$/);
  await page.getByRole("button", { name: "Close settings" }).click();
  await expect(page).toHaveURL(/\/record$/);
}

test.describe("Record — one final result and route-scoped presentation", () => {
  test("idle has no destination UI and every main-route start is explicitly Unfiled", async ({
    page,
  }) => {
    await openRecord(page);

    await expect(page.getByTestId("recording-result")).toHaveCount(0);
    await expect(page.getByTestId("recording-location-toggle")).toHaveCount(0);
    await expect(page.getByText("Save this recording in")).toHaveCount(0);
    await page.locator("button.start-btn").click();

    const starts = await page.evaluate(
      () => (window as unknown as { __starts?: unknown[] }).__starts ?? [],
    );
    expect(starts).toEqual([{ folderId: null }]);
  });

  test("Stop resolves to exactly one final card with navigation and compact filing", async ({
    page,
  }) => {
    await openRecord(page);
    await finishRecording(page);

    const result = page.getByTestId("recording-result");
    await expect(result).toHaveCount(1);
    await expect(result).toContainText("Saved");
    await expect(
      result.getByRole("link", { name: /Open saved (meeting|note)/i }),
    ).toHaveAttribute("href", "/meeting/m-rec");
    await expect(page.locator(".rec-strip")).toHaveCount(0);
    await expect(page.locator("app-meeting-conversation")).toHaveCount(0);
    await expect(page.locator("app-brain-reveal-card")).toHaveCount(0);
    await expect(page.locator("app-re-truth-card")).toHaveCount(1);

    const location = page.getByTestId("recording-location-toggle");
    await expect(location).toHaveText(/Unfiled/);
    await expect(page.getByTestId("recording-location-menu")).toHaveCount(0);
    await location.click();
    await expect(page.getByTestId("recording-location-menu")).toBeVisible();

    await expect(
      page.getByTestId("placement-destination-f-session-locked"),
    ).toBeDisabled();
    await expect(
      page.getByTestId("placement-destination-f-below-lock"),
    ).toBeDisabled();
    await expect(
      page.getByTestId("placement-destination-f-sealed"),
    ).toBeDisabled();
    await expect(
      page.getByTestId("placement-destination-f-hidden"),
    ).toHaveCount(0);
    await expect(
      page.getByTestId("placement-destination-notes-root"),
    ).toHaveCount(0);
    await expect(
      page.getByTestId("placement-destination-f-ideas"),
    ).toBeEnabled();

    await page.keyboard.press("Escape");
    await expect(page.getByTestId("recording-location-menu")).toHaveCount(0);
    await location.click();
    await page.getByTestId("placement-destination-f-weekly").click();
    await expect(location).toHaveText(/Acme \/ Weekly/);
    await expect(page.getByTestId("recording-location-menu")).toHaveCount(0);

    const moves = await page.evaluate(
      () => (window as unknown as { __moves?: unknown[] }).__moves ?? [],
    );
    expect(moves).toEqual([{ meetingId: "m-rec", folderId: "f-weekly" }]);
  });

  test("a filing failure stays inside the final card and retries the exact target", async ({
    page,
  }) => {
    await openRecord(page, {
      move_note: (args: unknown) => {
        const target = window as unknown as {
          __moves?: unknown[];
          __moveAttempt?: number;
        };
        (target.__moves ??= []).push(args);
        target.__moveAttempt = (target.__moveAttempt ?? 0) + 1;
        if (target.__moveAttempt === 1)
          throw new Error("temporary move failure");
        (
          target as typeof target & {
            __recordFolderId?: string;
          }
        ).__recordFolderId = (args as { folderId: string }).folderId;
        return null;
      },
    });
    await finishRecording(page);

    await page.getByTestId("recording-location-toggle").click();
    await page.getByTestId("placement-destination-f-weekly").click();
    const localError = page.getByTestId("recording-location-error");
    await expect(localError).toContainText(/couldn.t move/i);
    await expect(page.getByTestId("recording-result")).toBeVisible();

    await localError.getByRole("button", { name: /Try again/i }).click();
    await expect(page.getByTestId("recording-location-toggle")).toHaveText(
      /Acme \/ Weekly/,
    );
    const moves = await page.evaluate(
      () => (window as unknown as { __moves?: unknown[] }).__moves ?? [],
    );
    expect(moves).toEqual([
      { meetingId: "m-rec", folderId: "f-weekly" },
      { meetingId: "m-rec", folderId: "f-weekly" },
    ]);
  });

  test("location stays neutral while canonical detail resolves and offers retry when unavailable", async ({
    page,
  }) => {
    await openRecord(page, {
      get_meeting_detail: () => {
        const target = window as unknown as {
          __detailAttempts?: number;
          __rejectDetail?: () => void;
        };
        target.__detailAttempts = (target.__detailAttempts ?? 0) + 1;
        if (target.__detailAttempts === 2) {
          return new Promise((_resolve, reject) => {
            target.__rejectDetail = () =>
              reject(new Error("detail unavailable"));
          });
        }
        return {
          locked: false,
          meeting: {
            id: "m-rec",
            startedAt: "2026-08-27T09:00:00Z",
            endedAt: "2026-08-27T09:01:00Z",
            title: "Notes",
            durationS: 60,
            audioPath: null,
            status: "EXPORTED",
            folderId: null,
          },
          note: null,
          segments: [],
          assistantInteractions: [],
          aiProvider: null,
          aiModel: null,
          modelServed: null,
        };
      },
    });
    await finishRecording(page);

    const location = page.getByTestId("recording-location-toggle");
    await expect(location).toContainText("Checking location…");
    await expect(location).not.toContainText("Unfiled");
    await page.evaluate(() => {
      (window as unknown as { __rejectDetail?: () => void }).__rejectDetail?.();
    });
    await expect(location).toContainText("Location unavailable");
    await expect(location).not.toContainText("Unfiled");

    await location.click();
    await page.getByRole("button", { name: "Try again" }).click();
    await expect(location).toContainText("Unfiled");
  });

  test("a locked canonical detail masks its location and cannot open or move from the final card", async ({
    page,
  }) => {
    await openRecord(page, {
      get_meeting_detail: () => ({
        locked: true,
        meeting: {
          id: "m-rec",
          startedAt: "2026-08-27T09:00:00Z",
          endedAt: "2026-08-27T09:01:00Z",
          title: "",
          durationS: 60,
          audioPath: null,
          status: "EXPORTED",
          folderId: "f-weekly",
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
    });
    await finishRecording(page);

    const result = page.getByTestId("recording-result");
    const masked = page.getByTestId("recording-location-masked");
    await expect(masked).toContainText("Location hidden");
    await expect(masked).toContainText("Unlock its Workspace to view or change it");
    await expect(result).not.toContainText("Acme");
    await expect(result).not.toContainText("Weekly");
    await expect(result).not.toContainText("Unfiled");
    await expect(result).not.toContainText("Filed in");
    await expect(page.getByTestId("recording-location-toggle")).toHaveCount(0);
    await expect(page.getByTestId("recording-location-menu")).toHaveCount(0);
    await expect(
      page.locator("[data-testid^='placement-destination-']"),
    ).toHaveCount(0);

    const moves = await page.evaluate(
      () => (window as unknown as { __moves?: unknown[] }).__moves ?? [],
    );
    expect(moves).toEqual([]);
  });

  test("a privacy invalidation masks immediately and a delayed pre-lock detail cannot restore the label", async ({
    page,
  }) => {
    await openRecord(page, {
      get_meeting_detail: () => {
        const target = window as unknown as {
          __detailAttempts?: number;
          __resolveOldDetail?: () => void;
        };
        target.__detailAttempts = (target.__detailAttempts ?? 0) + 1;
        const detail = (locked: boolean, folderId: string) => ({
          locked,
          meeting: {
            id: "m-rec",
            startedAt: "2026-08-27T09:00:00Z",
            endedAt: "2026-08-27T09:01:00Z",
            title: locked ? "" : "Notes",
            durationS: 60,
            audioPath: null,
            status: "EXPORTED",
            folderId,
          },
          note: locked
            ? null
            : {
                meetingId: "m-rec",
                providerId: "claude_code",
                markdown: "# Notes",
                exportedPath: "/vault/Notes/m-rec.md",
              },
          segments: [],
          assistantInteractions: [],
          aiProvider: null,
          aiModel: null,
          modelServed: null,
        });
        // Store terminal hydration, then the placement's initial canonical read.
        if (target.__detailAttempts <= 2) return detail(false, "f-weekly");
        // Filing's pre-lock confirmation is deliberately held past invalidation.
        if (target.__detailAttempts === 3) {
          return new Promise((resolve) => {
            target.__resolveOldDetail = () => resolve(detail(false, "f-ideas"));
          });
        }
        return detail(true, "f-weekly");
      },
    });
    await finishRecording(page);

    const location = page.getByTestId("recording-location-toggle");
    await expect(location).toContainText("Acme / Weekly");
    await location.click();
    await page.getByTestId("placement-destination-f-ideas").click();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __detailAttempts?: number })
              .__detailAttempts ?? 0,
        ),
      )
      .toBe(3);

    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("murmur://reminder-visibility-invalidated", null);
    });
    const masked = page.getByTestId("recording-location-masked");
    await expect(masked).toContainText("Location hidden");
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __detailAttempts?: number })
              .__detailAttempts ?? 0,
        ),
      )
      .toBeGreaterThanOrEqual(4);

    await page.evaluate(() => {
      (
        window as unknown as { __resolveOldDetail?: () => void }
      ).__resolveOldDetail?.();
    });
    await expect(masked).toContainText("Location hidden");
    const result = page.getByTestId("recording-result");
    await expect(result).not.toContainText("Acme / Weekly");
    await expect(result).not.toContainText("Acme / Ideas");
    await expect(page.getByTestId("recording-location-menu")).toHaveCount(0);
    await expect(
      page.locator("[data-testid^='placement-destination-']"),
    ).toHaveCount(0);
  });

  test("privacy invalidation scrubs the terminal note, vault receipt, and Re-Truth synchronously", async ({
    page,
  }) => {
    await openRecord(page, {
      get_meeting_detail: () => ({
        locked: false,
        meeting: {
          id: "m-rec",
          startedAt: "2026-08-27T09:00:00Z",
          endedAt: "2026-08-27T09:01:00Z",
          title: "Notes",
          durationS: 60,
          audioPath: null,
          status: "EXPORTED",
          folderId: "f-weekly",
        },
        note: {
          meetingId: "m-rec",
          providerId: "claude_code",
          markdown: "# Private notes",
          exportedPath: "/vault/Notes/m-rec.md",
        },
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      preview_supersessions: () => {
        const target = window as unknown as {
          __retruthPreviewCalls?: number;
          __resolvePostLockPreview?: () => void;
        };
        target.__retruthPreviewCalls =
          (target.__retruthPreviewCalls ?? 0) + 1;
        if (target.__retruthPreviewCalls === 1) {
          return [
            {
              id: "sup-private",
              entity: "Project Atlas",
              predicate: "owner",
              oldValue: "Alice",
              newValue: "Bob",
              sourceNoteTitle: "Private roadmap",
              sourceNotePath: "/vault/Private roadmap.md",
              sourceMeetingId: "m-old",
              supersedingMeetingId: "m-rec",
              supersedingNoteTitle: "Notes",
              applied: false,
            },
          ];
        }
        return new Promise((resolve) => {
          target.__resolvePostLockPreview = () => resolve([]);
        });
      },
    });
    await finishRecording(page);

    const result = page.getByTestId("recording-result");
    await expect(result).toContainText("exported to your vault");
    await expect(page.getByText("Re-Truth · your vault moved on")).toBeVisible();

    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("murmur://reminder-visibility-invalidated", null);
    });

    await expect(result).toContainText("Saved safely in Murmur on this Mac");
    await expect(result).not.toContainText("exported to your vault");
    await expect(page.getByText("Re-Truth · your vault moved on")).toHaveCount(
      0,
    );

    // Re-activate the SAME mounted card without exposing a production test
    // hook. Angular's dev-mode inspector reaches the record component's public
    // store, whose existing refresh method restores only the mock's safe note.
    await page.evaluate(async () => {
      const ng = (
        window as unknown as {
          ng: { getComponent: (node: Element) => { store: { refreshLastNote: () => Promise<void> } } };
        }
      ).ng;
      const host = document.querySelector("app-record");
      if (!host) throw new Error("record host missing");
      await ng.getComponent(host).store.refreshLastNote();
    });
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __retruthPreviewCalls?: number })
              .__retruthPreviewCalls ?? 0,
        ),
      )
      .toBe(2);
    await expect(page.getByText("Project Atlas")).toHaveCount(0);
    await expect(page.getByText("Private roadmap")).toHaveCount(0);
    await page.evaluate(() => {
      (
        window as unknown as { __resolvePostLockPreview?: () => void }
      ).__resolvePostLockPreview?.();
    });
  });

  test("a delayed pre-lock terminal hydration cannot restore note content after privacy invalidation", async ({
    page,
  }) => {
    await openRecord(page, {
      get_meeting_detail: () => {
        const target = window as unknown as {
          __terminalDetailCalls?: number;
          __resolvePreLockTerminalDetail?: () => void;
        };
        target.__terminalDetailCalls =
          (target.__terminalDetailCalls ?? 0) + 1;
        const detail = (locked: boolean) => ({
          locked,
          meeting: {
            id: "m-rec",
            startedAt: "2026-08-27T09:00:00Z",
            endedAt: "2026-08-27T09:01:00Z",
            title: locked ? "" : "Private notes",
            durationS: 60,
            audioPath: null,
            status: "EXPORTED",
            folderId: "f-weekly",
          },
          note: locked
            ? null
            : {
                meetingId: "m-rec",
                providerId: "claude_code",
                markdown: "# Private notes",
                exportedPath: "/vault/Notes/m-rec.md",
              },
          segments: [],
          assistantInteractions: [],
          aiProvider: null,
          aiModel: null,
          modelServed: null,
        });
        if (target.__terminalDetailCalls === 1) {
          return new Promise((resolve) => {
            target.__resolvePreLockTerminalDetail = () =>
              resolve(detail(false));
          });
        }
        return detail(true);
      },
      preview_supersessions: () => [
        {
          id: "sup-private",
          entity: "Project Atlas",
          predicate: "owner",
          oldValue: "Alice",
          newValue: "Bob",
          sourceNoteTitle: "Private roadmap",
          sourceNotePath: "/vault/Private roadmap.md",
          sourceMeetingId: "m-old",
          supersedingMeetingId: "m-rec",
          supersedingNoteTitle: "Private notes",
          applied: false,
        },
      ],
    });
    await page.locator("button.start-btn").click();
    await page.locator("button.stop-btn").click();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __terminalDetailCalls?: number })
              .__terminalDetailCalls ?? 0,
        ),
      )
      .toBe(1);

    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("murmur://reminder-visibility-invalidated", null);
    });
    const result = page.getByTestId("recording-result");
    await expect(result).toContainText("Saved safely in Murmur on this Mac");
    await expect(result).not.toContainText("exported to your vault");
    await expect(page.getByTestId("recording-location-masked")).toBeVisible();

    await page.evaluate(() => {
      (
        window as unknown as { __resolvePreLockTerminalDetail?: () => void }
      ).__resolvePreLockTerminalDetail?.();
    });
    await page.waitForTimeout(100);
    await expect(result).toContainText("Saved safely in Murmur on this Mac");
    await expect(result).not.toContainText("exported to your vault");
    await expect(page.getByText("Re-Truth · your vault moved on")).toHaveCount(
      0,
    );
  });

  test("a delayed pre-lock Re-Truth preview cannot repopulate its cache after invalidation", async ({
    page,
  }) => {
    await openRecord(page, {
      get_meeting_detail: () => ({
        locked: false,
        meeting: {
          id: "m-rec",
          startedAt: "2026-08-27T09:00:00Z",
          endedAt: "2026-08-27T09:01:00Z",
          title: "Notes",
          durationS: 60,
          audioPath: null,
          status: "EXPORTED",
          folderId: "f-weekly",
        },
        note: {
          meetingId: "m-rec",
          providerId: "claude_code",
          markdown: "# Notes",
          exportedPath: "/vault/Notes/m-rec.md",
        },
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      preview_supersessions: () => {
        const target = window as unknown as {
          __delayedPreviewCalls?: number;
          __resolvePreLockPreview?: () => void;
          __resolveSafePreview?: () => void;
        };
        target.__delayedPreviewCalls =
          (target.__delayedPreviewCalls ?? 0) + 1;
        if (target.__delayedPreviewCalls === 1) {
          return new Promise((resolve) => {
            target.__resolvePreLockPreview = () =>
              resolve([
                {
                  id: "sup-delayed-private",
                  entity: "Secret Entity",
                  predicate: "decision",
                  oldValue: "Private old value",
                  newValue: "Private new value",
                  sourceNoteTitle: "Secret source note",
                  sourceNotePath: "/vault/Secret source note.md",
                  sourceMeetingId: "m-old",
                  supersedingMeetingId: "m-rec",
                  supersedingNoteTitle: "Notes",
                  applied: false,
                },
              ]);
          });
        }
        return new Promise((resolve) => {
          target.__resolveSafePreview = () => resolve([]);
        });
      },
    });
    await finishRecording(page);
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __delayedPreviewCalls?: number })
              .__delayedPreviewCalls ?? 0,
        ),
      )
      .toBe(1);

    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("murmur://reminder-visibility-invalidated", null);
      (
        window as unknown as { __resolvePreLockPreview?: () => void }
      ).__resolvePreLockPreview?.();
    });

    await page.evaluate(async () => {
      const ng = (
        window as unknown as {
          ng: { getComponent: (node: Element) => { store: { refreshLastNote: () => Promise<void> } } };
        }
      ).ng;
      const host = document.querySelector("app-record");
      if (!host) throw new Error("record host missing");
      await ng.getComponent(host).store.refreshLastNote();
    });
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __delayedPreviewCalls?: number })
              .__delayedPreviewCalls ?? 0,
        ),
      )
      .toBe(2);
    await expect(page.getByText("Secret Entity")).toHaveCount(0);
    await expect(page.getByText("Private old value")).toHaveCount(0);
    await expect(page.getByText("Secret source note")).toHaveCount(0);
    await page.evaluate(() => {
      (
        window as unknown as { __resolveSafePreview?: () => void }
      ).__resolveSafePreview?.();
    });
  });

  test("a delayed filing rejection after privacy invalidation cannot restore its destination label or error", async ({
    page,
  }) => {
    await openRecord(page, {
      get_meeting_detail: () => {
        const target = window as unknown as { __privacyLocked?: boolean };
        const locked = target.__privacyLocked === true;
        return {
          locked,
          meeting: {
            id: "m-rec",
            startedAt: "2026-08-27T09:00:00Z",
            endedAt: "2026-08-27T09:01:00Z",
            title: locked ? "" : "Notes",
            durationS: 60,
            audioPath: null,
            status: "EXPORTED",
            folderId: "f-weekly",
          },
          note: locked
            ? null
            : {
                meetingId: "m-rec",
                providerId: "claude_code",
                markdown: "# Notes",
                exportedPath: "/vault/Notes/m-rec.md",
              },
          segments: [],
          assistantInteractions: [],
          aiProvider: null,
          aiModel: null,
          modelServed: null,
        };
      },
      move_note: (args: unknown) => {
        const target = window as unknown as {
          __moves?: unknown[];
          __rejectMove?: () => void;
        };
        (target.__moves ??= []).push(args);
        return new Promise((_resolve, reject) => {
          target.__rejectMove = () => reject(new Error("pre-lock failure"));
        });
      },
    });
    await finishRecording(page);

    await page.getByTestId("recording-location-toggle").click();
    await page.getByTestId("placement-destination-f-ideas").click();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { __moves?: unknown[] }).__moves?.length,
        ),
      )
      .toBe(1);
    await page.evaluate(() => {
      const target = window as unknown as {
        __privacyLocked?: boolean;
        __demoEmit: (event: string, payload: unknown) => void;
      };
      target.__privacyLocked = true;
      target.__demoEmit("murmur://reminder-visibility-invalidated", null);
    });
    await expect(page.getByTestId("recording-location-masked")).toBeVisible();
    await page.evaluate(() => {
      (window as unknown as { __rejectMove?: () => void }).__rejectMove?.();
    });

    const result = page.getByTestId("recording-result");
    await expect(result).toContainText("Location hidden");
    await expect(result).not.toContainText("Ideas");
    await expect(result).not.toContainText("Couldn’t move");
    await expect(page.getByTestId("recording-location-error")).toHaveCount(0);
    await expect(page.getByTestId("recording-location-menu")).toHaveCount(0);
  });

  test("an empty destination forest loads once, stays calm, and retries only on request", async ({
    page,
  }) => {
    await openRecord(page, {
      list_workspace_tree: () => {
        const target = window as unknown as { __treeReads?: number };
        target.__treeReads = (target.__treeReads ?? 0) + 1;
        return [];
      },
    });

    await finishRecording(page);
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __treeReads?: number }).__treeReads ?? 0,
        ),
      )
      .toBe(1);
    await page.waitForTimeout(350);
    expect(
      await page.evaluate(
        () => (window as unknown as { __treeReads?: number }).__treeReads ?? 0,
      ),
    ).toBe(1);

    await page.getByTestId("recording-location-toggle").click();
    await expect(page.getByText("No open locations match.")).toBeVisible();
    await page.getByRole("button", { name: "Refresh locations" }).click();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __treeReads?: number }).__treeReads ?? 0,
        ),
      )
      .toBe(2);
    await page.waitForTimeout(350);
    expect(
      await page.evaluate(
        () => (window as unknown as { __treeReads?: number }).__treeReads ?? 0,
      ),
    ).toBe(2);
  });

  test("a status-only completion stays busy at saved and hydrates the exact meeting only after finalized", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => true,
      get_last_note: () => ({
        meetingId: "m-old",
        providerId: "claude_code",
        markdown: "# Old",
        exportedPath: "/vault/Old.md",
      }),
      get_meeting_detail: () => {
        const target = window as unknown as {
          __terminalDetail?: unknown;
          __terminalDetailCalls?: number;
          __resolveTerminalDetail?: () => void;
        };
        target.__terminalDetailCalls = (target.__terminalDetailCalls ?? 0) + 1;
        target.__terminalDetail ??= {
          locked: false,
          meeting: {
            id: "m-cross-window",
            startedAt: "2026-08-27T10:00:00Z",
            endedAt: "2026-08-27T10:01:00Z",
            title: "New note",
            durationS: 60,
            audioPath: null,
            status: "SUMMARIZED",
            folderId: null,
          },
          note: {
            meetingId: "m-cross-window",
            providerId: "claude_code",
            markdown: "# New note",
            exportedPath: null,
          },
          segments: [],
          assistantInteractions: [],
          aiProvider: null,
          aiModel: null,
          modelServed: null,
        };
        if (target.__terminalDetailCalls === 1) {
          return new Promise((resolve) => {
            target.__resolveTerminalDetail = () =>
              resolve(target.__terminalDetail);
          });
        }
        return target.__terminalDetail;
      },
    });
    await page.goto("/record");
    await expect(page.locator("button.start-btn")).toBeVisible();

    await page.evaluate(() => {
      const emit = (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit;
      emit("meetnotes://status", {
        stage: "recording",
        message: "Recording",
        meetingId: "m-cross-window",
      });
      emit("meetnotes://status", {
        stage: "saved",
        message: "Saved to Murmur.",
        meetingId: "m-cross-window",
      });
    });
    await expect(page.locator(".proc-inline")).toContainText("Saved to Murmur");
    await expect(page.getByTestId("recording-result")).toHaveCount(0);
    await expect(page.getByText("exported to your vault")).toHaveCount(0);

    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("meetnotes://status", {
        stage: "finalized",
        message: "Recording finalized.",
        meetingId: "m-cross-window",
      });
    });
    await expect(page.locator(".proc-inline")).toBeVisible();
    await expect(page.getByTestId("recording-result")).toHaveCount(0);

    await page.evaluate(() => {
      (
        window as unknown as { __resolveTerminalDetail?: () => void }
      ).__resolveTerminalDetail?.();
    });
    const result = page.getByTestId("recording-result");
    await expect(result).toBeVisible();
    await expect(result).toContainText("Saved safely in Murmur");
    await expect(result).not.toContainText("exported to your vault");
    await expect(
      result.getByRole("link", { name: "Open saved meeting" }),
    ).toHaveAttribute("href", "/meeting/m-cross-window");
  });

  test("a local StopResult cannot regress an already-finalized canonical result back to processing", async ({
    page,
  }) => {
    await openRecord(page, {
      stop_recording: () =>
        new Promise((resolve) => {
          (
            window as unknown as { __finishFinalizedStop?: () => void }
          ).__finishFinalizedStop = () =>
            resolve({
              meetingId: "m-rec",
              markdown: "# Notes",
              exportedPath: null,
            });
        }),
      get_meeting_detail: () => {
        const target = window as unknown as { __detailCalls?: number };
        target.__detailCalls = (target.__detailCalls ?? 0) + 1;
        return {
          locked: false,
          meeting: {
            id: "m-rec",
            startedAt: "2026-08-27T09:00:00Z",
            endedAt: "2026-08-27T09:01:00Z",
            title: "Notes",
            durationS: 60,
            audioPath: null,
            status: "SUMMARIZED",
            folderId: null,
          },
          note: {
            meetingId: "m-rec",
            providerId: "claude_code",
            markdown: "# Notes",
            exportedPath: null,
          },
          segments: [],
          assistantInteractions: [],
          aiProvider: null,
          aiModel: null,
          modelServed: null,
        };
      },
    });
    await page.locator("button.start-btn").click();
    await page.locator("button.stop-btn").click();
    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("meetnotes://status", {
        stage: "finalized",
        message: "Recording finalized.",
        meetingId: "m-rec",
      });
    });
    await expect(page.getByTestId("recording-result")).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { __detailCalls?: number }).__detailCalls,
        ),
      )
      .toBe(2);

    await page.evaluate(() => {
      (
        window as unknown as { __finishFinalizedStop?: () => void }
      ).__finishFinalizedStop?.();
    });
    await page.waitForTimeout(350);
    await expect(page.getByTestId("recording-result")).toBeVisible();
    await expect(page.locator(".proc-inline")).toHaveCount(0);
    expect(
      await page.evaluate(
        () => (window as unknown as { __detailCalls?: number }).__detailCalls,
      ),
    ).toBe(2);

    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("meetnotes://status", {
        stage: "finalized",
        message: "Duplicate finalization.",
        meetingId: "m-rec",
      });
    });
    await page.waitForTimeout(100);
    await expect(page.getByTestId("recording-result")).toBeVisible();
    await expect(page.locator(".proc-inline")).toHaveCount(0);
    expect(
      await page.evaluate(
        () => (window as unknown as { __detailCalls?: number }).__detailCalls,
      ),
    ).toBe(2);
  });

  test("a failed terminal detail read settles to a safe result instead of wedging on Saved", async ({
    page,
  }) => {
    await openRecord(page, {
      get_meeting_detail: () => {
        const target = window as unknown as { __detailCalls?: number };
        target.__detailCalls = (target.__detailCalls ?? 0) + 1;
        if (target.__detailCalls === 1) {
          throw new Error("terminal detail unavailable");
        }
        return {
          locked: false,
          meeting: {
            id: "m-rec",
            startedAt: "2026-08-27T09:00:00Z",
            endedAt: "2026-08-27T09:01:00Z",
            title: "Notes",
            durationS: 60,
            audioPath: null,
            status: "EXPORTED",
            folderId: null,
          },
          note: null,
          segments: [],
          assistantInteractions: [],
          aiProvider: null,
          aiModel: null,
          modelServed: null,
        };
      },
    });
    await finishRecording(page);

    const result = page.getByTestId("recording-result");
    await expect(result).toContainText("Saved safely in Murmur on this Mac");
    await expect(result).not.toContainText("exported to your vault");
    await expect(page.locator(".proc-inline")).toHaveCount(0);
    await expect(
      result.getByRole("link", { name: "Open saved meeting" }),
    ).toHaveAttribute("href", "/meeting/m-rec");
  });

  test("done renders only the final result and optional Re-Truth, even when every pre-result notice is eligible", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => {
        const target = window as unknown as {
          __modelPresentCalls?: number;
          __modelMissingNow?: boolean;
        };
        target.__modelPresentCalls = (target.__modelPresentCalls ?? 0) + 1;
        return target.__modelMissingNow !== true;
      },
      output_is_builtin_speakers: () => true,
      start_recording: () => ({ meetingId: "m-notices" }),
      stop_recording: () => ({
        meetingId: "m-notices",
        markdown: "# Note",
        exportedPath: null,
      }),
      get_meeting_detail: () => ({
        locked: false,
        meeting: {
          id: "m-notices",
          startedAt: "2026-08-27T10:00:00Z",
          endedAt: "2026-08-27T10:01:00Z",
          title: "Note",
          durationS: 60,
          audioPath: null,
          status: "SUMMARIZED",
          folderId: null,
        },
        note: {
          meetingId: "m-notices",
          providerId: "claude_code",
          markdown: "# Note",
          exportedPath: null,
        },
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
    });
    await page.addInitScript(() => {
      (
        window as unknown as { __demoConfig: Record<string, unknown> }
      ).__demoConfig = {
        vaultPath: "",
        liveCaptions: "modelMissing",
        captureSystemAudio: true,
      };
    });
    await page.goto("/record");
    await expect(page.locator(".vault-notice")).toBeVisible();
    await expect(page.locator(".cc-notice")).toBeVisible();
    await expect(page.getByText(/Capturing system audio/)).toBeVisible();

    await finishRecording(page);
    await expect(page.locator(".model-banner")).toHaveCount(0);
    await expect(page.locator(".vault-notice")).toHaveCount(0);
    await expect(page.locator(".cc-notice")).toHaveCount(0);
    await expect(page.getByText(/Capturing system audio/)).toHaveCount(0);
    await expect(page.locator(".rec-strip")).toHaveCount(0);
    await expect(page.locator("app-meeting-conversation")).toHaveCount(0);
    await expect(page.getByTestId("recording-result")).toHaveCount(1);
    await expect(page.locator("app-re-truth-card")).toHaveCount(1);

    await page.evaluate(() => {
      const target = window as unknown as {
        __modelMissingNow?: boolean;
        __demoEmit: (event: string, payload: unknown) => void;
      };
      target.__modelMissingNow = true;
      target.__demoEmit("murmur://model-download", { done: true });
    });
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __modelPresentCalls?: number })
              .__modelPresentCalls ?? 0,
        ),
      )
      .toBeGreaterThanOrEqual(2);
    await expect(page.locator(".model-banner")).toHaveCount(0);
    await expect(page.getByTestId("recording-result")).toHaveCount(1);
  });

  test("leaving a done or error visit clears terminal UI before returning", async ({
    page,
  }) => {
    await openRecord(page);
    await finishRecording(page);
    await navigateAwayAndBack(page);

    await expect(page.getByTestId("recording-result")).toHaveCount(0);
    await expect(page.locator("app-re-truth-card")).toHaveCount(0);
    await expect(page.locator("button.start-btn")).toBeVisible();

    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("meetnotes://status", {
        stage: "error",
        message: "[recording] failed locally",
        meetingId: "m-error",
      });
    });
    await expect(page.getByRole("alert")).toContainText(
      /Couldn.t finish that recording/i,
    );
    await navigateAwayAndBack(page);
    await expect(page.getByRole("alert")).toHaveCount(0);
    await expect(page.locator("button.start-btn")).toBeVisible();
  });

  test("route exit preserves recording and processing, but a completion while away returns to clean idle", async ({
    page,
  }) => {
    await openRecord(page, {
      stop_recording: () =>
        new Promise((resolve) => {
          (
            window as unknown as {
              __finishStop?: () => void;
            }
          ).__finishStop = () =>
            resolve({
              meetingId: "m-rec",
              markdown: "# Notes",
              exportedPath: "/vault/Notes/m-rec.md",
            });
        }),
      get_last_note: () => {
        const target = window as unknown as { __lastNoteCalls?: number };
        target.__lastNoteCalls = (target.__lastNoteCalls ?? 0) + 1;
        return {
          meetingId: "m-rec",
          providerId: "claude_code",
          markdown: "# Notes",
          exportedPath: "/vault/Notes/m-rec.md",
        };
      },
      get_meeting_detail: () => {
        const target = window as unknown as {
          __detailCalls?: number;
          __resolveTerminalDetail?: () => void;
        };
        target.__detailCalls = (target.__detailCalls ?? 0) + 1;
        const detail = {
          locked: false,
          meeting: {
            id: "m-rec",
            startedAt: "2026-08-27T09:00:00Z",
            endedAt: "2026-08-27T09:01:00Z",
            title: "Notes",
            durationS: 60,
            audioPath: null,
            status: "EXPORTED",
            folderId: null,
          },
          note: {
            meetingId: "m-rec",
            providerId: "claude_code",
            markdown: "# Notes",
            exportedPath: "/vault/Notes/m-rec.md",
          },
          segments: [],
          assistantInteractions: [],
          aiProvider: null,
          aiModel: null,
          modelServed: null,
        };
        return new Promise((resolve) => {
          target.__resolveTerminalDetail = () => resolve(detail);
        });
      },
    });

    await page.locator("button.start-btn").click();
    await navigateAwayAndBack(page);
    await expect(page.locator("button.stop-btn")).toBeVisible();

    await page.locator("button.stop-btn").click();
    await expect(page.locator(".proc-inline")).toBeVisible();
    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("meetnotes://status", {
        stage: "saved",
        message: "Saved to Murmur — finishing up…",
        meetingId: "m-rec",
      });
    });
    await expect(page.locator(".proc-inline")).toContainText("Saved to Murmur");
    await navigateAwayAndBack(page);
    await expect(page.locator(".proc-inline")).toContainText("Saved to Murmur");
    await expect(page.locator("button.start-btn")).toHaveCount(0);
    await page.getByRole("link", { name: "Settings" }).click();
    await expect(page).toHaveURL(/\/settings$/);
    await page.evaluate(() => {
      (
        window as unknown as {
          __finishStop?: () => void;
        }
      ).__finishStop?.();
    });
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __detailCalls?: number }).__detailCalls ??
            0,
        ),
      )
      .toBeGreaterThanOrEqual(1);
    await page.getByRole("button", { name: "Close settings" }).click();
    await expect(page).toHaveURL(/\/record$/);
    await expect(page.getByTestId("recording-result")).toHaveCount(0);
    await expect(page.locator(".proc-inline")).toHaveCount(0);
    await expect(page.locator("button.start-btn")).toHaveText(
      /Start recording/,
    );
    await expect(page.getByText("Save this recording in")).toHaveCount(0);
    await page.evaluate(() => {
      (
        window as unknown as { __resolveTerminalDetail?: () => void }
      ).__resolveTerminalDetail?.();
    });
    await page.waitForTimeout(100);
    await expect(page.getByTestId("recording-result")).toHaveCount(0);
    await expect(page.locator("button.start-btn")).toHaveText(
      /Start recording/,
    );
  });

  test("a terminal hydration started on Record cannot leave Saved wedged after resolving away", async ({
    page,
  }) => {
    await openRecord(page, {
      get_meeting_detail: () =>
        new Promise((resolve) => {
          (
            window as unknown as {
              __awayDetailStarted?: boolean;
              __resolveAwayDetail?: () => void;
            }
          ).__awayDetailStarted = true;
          (
            window as unknown as { __resolveAwayDetail?: () => void }
          ).__resolveAwayDetail = () =>
            resolve({
              locked: false,
              meeting: {
                id: "m-away",
                startedAt: "2026-08-27T09:00:00Z",
                endedAt: "2026-08-27T09:01:00Z",
                title: "Away",
                durationS: 60,
                audioPath: null,
                status: "EXPORTED",
                folderId: null,
              },
              note: {
                meetingId: "m-away",
                providerId: "claude_code",
                markdown: "# Away",
                exportedPath: "/vault/Away.md",
              },
              segments: [],
              assistantInteractions: [],
              aiProvider: null,
              aiModel: null,
              modelServed: null,
            });
        }),
    });
    await page.evaluate(() => {
      const emit = (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit;
      emit("meetnotes://status", {
        stage: "saved",
        message: "Saved to Murmur.",
        meetingId: "m-away",
      });
      emit("meetnotes://status", {
        stage: "finalized",
        message: "Recording finalized.",
        meetingId: "m-away",
      });
    });
    await expect(page.locator(".proc-inline")).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __awayDetailStarted?: boolean })
              .__awayDetailStarted ?? false,
        ),
      )
      .toBe(true);
    await page.getByRole("link", { name: "Settings" }).click();
    await page.evaluate(() => {
      (
        window as unknown as { __resolveAwayDetail?: () => void }
      ).__resolveAwayDetail?.();
    });
    await page.waitForTimeout(100);
    await page.getByRole("button", { name: "Close settings" }).click();

    await expect(page.locator("button.start-btn")).toHaveText(
      /Start recording/,
    );
    await expect(page.locator(".proc-inline")).toHaveCount(0);
    await expect(page.getByTestId("recording-result")).toHaveCount(0);
  });

  test("a late Stop settlement cannot resurrect a terminal meeting retired while away", async ({
    page,
  }) => {
    await openRecord(page, {
      stop_recording: () =>
        new Promise((resolve) => {
          (window as unknown as { __resolveLateStop?: () => void })
            .__resolveLateStop = () => resolve({ meetingId: "m-rec" });
        }),
      get_meeting_detail: () => {
        const target = window as unknown as { __awayFinalDetailCalls?: number };
        target.__awayFinalDetailCalls =
          (target.__awayFinalDetailCalls ?? 0) + 1;
        return {
          locked: false,
          meeting: {
            id: "m-rec",
            startedAt: "2026-08-27T09:00:00Z",
            endedAt: "2026-08-27T09:01:00Z",
            title: "Notes",
            durationS: 60,
            audioPath: null,
            status: "EXPORTED",
            folderId: null,
          },
          note: {
            meetingId: "m-rec",
            providerId: "claude_code",
            markdown: "# Notes",
            exportedPath: "/vault/Notes/m-rec.md",
          },
          segments: [],
          assistantInteractions: [],
          aiProvider: null,
          aiModel: null,
          modelServed: null,
        };
      },
    });
    await page.locator("button.start-btn").click();
    await page.locator("button.stop-btn").click();
    await page.getByRole("link", { name: "Settings" }).click();
    await expect(page).toHaveURL(/\/settings$/);
    await expect(page.locator("app-record")).toHaveCount(0);
    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("meetnotes://status", {
        stage: "finalized",
        message: "Recording finalized.",
        meetingId: "m-rec",
      });
    });
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __awayFinalDetailCalls?: number })
              .__awayFinalDetailCalls ?? 0,
        ),
      )
      .toBeGreaterThanOrEqual(1);
    await page.waitForTimeout(100);
    await page.getByRole("button", { name: "Close settings" }).click();
    await expect(page.locator("button.start-btn")).toBeVisible();

    await page.evaluate(() => {
      (window as unknown as { __resolveLateStop?: () => void })
        .__resolveLateStop?.();
    });
    await page.waitForTimeout(100);
    await expect(page.locator("button.start-btn")).toBeVisible();
    await expect(page.locator(".proc-inline")).toHaveCount(0);
    await expect(page.getByTestId("recording-result")).toHaveCount(0);
  });

  test("an old Stop settlement cannot replace a newer cross-window recording", async ({
    page,
  }) => {
    await openRecord(page, {
      stop_recording: () =>
        new Promise((resolve) => {
          (window as unknown as { __resolveOldStop?: () => void })
            .__resolveOldStop = () => resolve({ meetingId: "m-rec" });
        }),
      get_meeting_detail: () => ({
        locked: false,
        meeting: {
          id: "m-rec",
          startedAt: "2026-08-27T09:00:00Z",
          endedAt: "2026-08-27T09:01:00Z",
          title: "Old recording",
          durationS: 60,
          audioPath: null,
          status: "EXPORTED",
          folderId: null,
        },
        note: {
          meetingId: "m-rec",
          providerId: "claude_code",
          markdown: "# Old recording",
          exportedPath: null,
        },
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
    });
    await page.locator("button.start-btn").click();
    await page.locator("button.stop-btn").click();
    await page.evaluate(() => {
      const emit = (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit;
      emit("meetnotes://status", {
        stage: "finalized",
        message: "Old recording finalized.",
        meetingId: "m-rec",
      });
    });
    await expect(page.getByTestId("recording-result")).toBeVisible();

    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("meetnotes://status", {
        stage: "recording",
        message: "New recording",
        meetingId: "m-new",
      });
    });
    await expect(page.locator("button.stop-btn")).toBeVisible();
    await page.evaluate(() => {
      (window as unknown as { __resolveOldStop?: () => void })
        .__resolveOldStop?.();
    });
    await page.waitForTimeout(100);

    await expect(page.locator("button.stop-btn")).toBeVisible();
    await expect(page.locator(".proc-inline")).toHaveCount(0);
    await expect(page.getByTestId("recording-result")).toHaveCount(0);
  });

  test("an old Stop rejection cannot turn a newer cross-window recording into an error", async ({
    page,
  }) => {
    await openRecord(page, {
      stop_recording: () =>
        new Promise((_resolve, reject) => {
          (window as unknown as { __rejectOldStop?: () => void })
            .__rejectOldStop = () => reject(new Error("old stop failed"));
        }),
    });
    await page.locator("button.start-btn").click();
    await page.locator("button.stop-btn").click();
    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("meetnotes://status", {
        stage: "recording",
        message: "New recording",
        meetingId: "m-new",
      });
    });
    await expect(page.locator("button.stop-btn")).toBeVisible();

    await page.evaluate(() => {
      (window as unknown as { __rejectOldStop?: () => void })
        .__rejectOldStop?.();
    });
    await page.waitForTimeout(100);

    await expect(page.locator("button.stop-btn")).toBeVisible();
    await expect(page.getByRole("alert")).toHaveCount(0);
    await expect(page.locator(".proc-inline")).toHaveCount(0);
  });
});
