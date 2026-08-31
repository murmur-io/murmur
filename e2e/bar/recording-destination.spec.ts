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

const FILED_FOREST = [
  container("p-acme", "Acme", "project", false, false, [
    {
      ...container("f-weekly", "Weekly", "folder", false, false),
      groups: [
        {
          kind: "meeting",
          total: 1,
          items: [
            {
              kind: "meeting",
              id: "m-bar-filed",
              title: "Weekly sync",
              durationS: 42,
              sortAt: 1_787_824_800_000,
            },
          ],
        },
      ],
    },
  ]),
];

const REOPENED_FOREST = [
  container("p-acme", "Acme HQ", "project", false, false, [
    container("f-fresh", "Fresh", "folder", false, false),
  ]),
  container("p-roadmap", "Roadmap", "project", false, false, [
    container("f-weekly", "Planning", "folder", false, false),
  ]),
];

interface BrowserFailures {
  readonly consoleErrors: string[];
  readonly pageErrors: string[];
}

async function openBar(page: Page): Promise<BrowserFailures> {
  const failures: BrowserFailures = { consoleErrors: [], pageErrors: [] };
  page.on("console", (message) => {
    if (message.type() === "error") failures.consoleErrors.push(message.text());
  });
  page.on("pageerror", (error) => failures.pageErrors.push(error.message));
  await page.setViewportSize({ width: 540, height: 58 });
  await page.addInitScript(
    ({ forest, filedForest }) => {
      const phase = localStorage.getItem("murmur.e2e.barRecordingPhase");
      (
        window as unknown as {
          __barForest: unknown[];
          __barFiledForest: unknown[];
          __barTreeReads: number;
          __barTreeFails: boolean;
        }
      ).__barForest = phase === "done" ? filedForest : forest;
      (window as unknown as { __barFiledForest: unknown[] }).__barFiledForest =
        filedForest;
      (window as unknown as { __barTreeReads: number }).__barTreeReads = 0;
      (window as unknown as { __barTreeFails: boolean }).__barTreeFails = false;
    },
    { forest: FOREST, filedForest: FILED_FOREST },
  );
  await mockTauri(
    page,
    {
      start_recording: (args: unknown) => {
        const target = window as unknown as { __starts?: unknown[] };
        (target.__starts ??= []).push(args);
        localStorage.setItem("murmur.e2e.barRecordingPhase", "recording");
        return {
          meetingId: "m-bar-filed",
          startedAt: "2026-08-27T09:00:00Z",
        };
      },
      recording_status: () => ({
        recording:
          localStorage.getItem("murmur.e2e.barRecordingPhase") === "recording",
        meetingId:
          localStorage.getItem("murmur.e2e.barRecordingPhase") === "recording"
            ? "m-bar-filed"
            : null,
        startedAt: "2026-08-27T09:00:00Z",
      }),
      stop_recording: () => {
        const target = window as unknown as {
          __barForest: unknown[];
          __barFiledForest: unknown[];
        };
        localStorage.setItem("murmur.e2e.barRecordingPhase", "done");
        target.__barForest = target.__barFiledForest;
        return {
          meetingId: "m-bar-filed",
          markdown: "# Weekly sync",
          exportedPath: "/vault/Acme/Weekly/m-bar-filed.md",
        };
      },
      get_last_note: () => ({
        meetingId: "m-bar-filed",
        providerId: "claude_code",
        markdown: "# Weekly sync",
        exportedPath: "/vault/Acme/Weekly/m-bar-filed.md",
      }),
      get_meeting_detail: () => ({
        locked: false,
        meeting: {
          id: "m-bar-filed",
          startedAt: "2026-08-27T09:00:00Z",
          endedAt: "2026-08-27T09:00:42Z",
          title: "Weekly sync",
          durationS: 42,
          audioPath: null,
          status: "EXPORTED",
          folderId:
            localStorage.getItem("murmur.e2e.barRecordingPhase") === "done"
              ? "f-weekly"
              : null,
        },
        note: null,
        segments: [],
        assistantInteractions: [],
        aiProvider: null,
        aiModel: null,
        modelServed: null,
      }),
      list_container_items: () => ({
        kind: "meeting",
        items: [],
        total: 0,
      }),
      list_workspace_tree: () => {
        const target = window as unknown as {
          __barForest: unknown[];
          __barTreeReads: number;
          __barTreeFails: boolean;
        };
        target.__barTreeReads += 1;
        if (target.__barTreeFails) throw new Error("tree unavailable");
        return target.__barForest;
      },
    },
    {
      recording_status: { recording: false, meetingId: null, startedAt: null },
    },
  );
  await page.goto("/bar");
  await expect(page.getByTestId("bar-recording-destination")).toBeVisible();
  return failures;
}

async function starts(page: Page): Promise<unknown[]> {
  return page.evaluate(
    () => (window as unknown as { __starts?: unknown[] }).__starts ?? [],
  );
}

test.describe("Floating bar — recording destination", () => {
  test("starts explicitly Unfiled and fits the native 540×58 window", async ({
    page,
  }) => {
    const failures = await openBar(page);
    const picker = page.getByTestId("bar-recording-destination");

    await expect(picker).toHaveValue("");
    await expect(picker.locator("option").first()).toHaveText(
      /Unfiled · Default/,
    );
    await page.getByRole("button", { name: "Start recording" }).click();

    expect(await starts(page)).toEqual([{ folderId: null }]);
    const viewportFit = await page.evaluate(() => ({
      scrollWidth: document.documentElement.scrollWidth,
      scrollHeight: document.documentElement.scrollHeight,
      width: window.innerWidth,
      height: window.innerHeight,
    }));
    expect(viewportFit.scrollWidth).toBeLessThanOrEqual(viewportFit.width);
    expect(viewportFit.scrollHeight).toBeLessThanOrEqual(viewportFit.height);
    expect(failures).toEqual({ consoleErrors: [], pageErrors: [] });
  });

  test("sends the exact open folder ID with a Workspace/Folder breadcrumb", async ({
    page,
  }) => {
    const failures = await openBar(page);
    const picker = page.getByTestId("bar-recording-destination");

    await expect(picker.locator("option[value='p-acme']")).toContainText(
      "Workspace · Acme",
    );
    await expect(picker.locator("option[value='f-weekly']")).toContainText(
      "Folder · Acme / Weekly",
    );
    await picker.selectOption("f-weekly");
    await page.getByRole("button", { name: "Start recording" }).click();

    expect(await starts(page)).toEqual([{ folderId: "f-weekly" }]);
    expect(failures).toEqual({ consoleErrors: [], pageErrors: [] });
  });

  test("final card reads the bar-started recording's current location from the canonical tree", async ({
    page,
  }) => {
    const failures = await openBar(page);
    await page
      .getByTestId("bar-recording-destination")
      .selectOption("f-weekly");
    await page.getByRole("button", { name: "Start recording" }).click();
    expect(await starts(page)).toEqual([{ folderId: "f-weekly" }]);

    // The native app opens `/record` in the main webview. A new Angular store
    // reconciles the still-running capture from Rust, then the final card must
    // read placement from the canonical workspace tree after Stop.
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.goto("/record");
    await expect(page.locator("button.stop-btn")).toBeVisible();
    await page.locator("button.stop-btn").click();

    const location = page.getByTestId("recording-location-toggle");
    await expect(page.getByTestId("recording-result")).toBeVisible();
    await expect(location).toContainText("Acme / Weekly");
    await expect(location).not.toContainText("Unfiled");
    expect(failures).toEqual({ consoleErrors: [], pageErrors: [] });
  });

  test("disables every locked ancestry path and hides sealed descendants", async ({
    page,
  }) => {
    const failures = await openBar(page);
    const picker = page.getByTestId("bar-recording-destination");

    await expect(
      picker.locator("option[value='f-session-locked']"),
    ).toHaveAttribute("disabled", "");
    await expect(
      picker.locator("option[value='f-below-lock']"),
    ).toHaveAttribute("disabled", "");
    await expect(picker.locator("option[value='f-sealed']")).toHaveAttribute(
      "disabled",
      "",
    );
    await expect(picker.locator("option[value='f-hidden']")).toHaveCount(0);
    await expect(picker.locator("option[value='notes-root']")).toHaveCount(0);
    await expect(picker.locator("option[value='f-ideas']")).toBeEnabled();
    await expect(
      picker.locator("option[value='f-session-locked']"),
    ).toContainText("Locked");
    expect(await starts(page)).toEqual([]);
    expect(failures).toEqual({ consoleErrors: [], pageErrors: [] });
  });

  test("voice and tray starts stay explicitly Unfiled despite a picker selection", async ({
    page,
  }) => {
    const failures = await openBar(page);
    await page
      .getByTestId("bar-recording-destination")
      .selectOption("f-weekly");

    await page.evaluate(() => {
      (
        window as unknown as {
          __demoEmit: (event: string, payload: unknown) => void;
        }
      ).__demoEmit("murmur://voice-start", null);
    });
    await expect.poll(() => starts(page)).toEqual([{ folderId: null }]);

    await page.evaluate(() => {
      const target = window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      };
      target.__demoEmit("meetnotes://status", {
        stage: "idle",
        message: "",
        meetingId: null,
      });
      target.__demoEmit("murmur://toggle-record", null);
    });
    await expect
      .poll(() => starts(page))
      .toEqual([{ folderId: null }, { folderId: null }]);
    expect(failures).toEqual({ consoleErrors: [], pageErrors: [] });
  });

  test("privacy invalidation scrubs a selected destination even when refresh fails", async ({
    page,
  }) => {
    const failures = await openBar(page);
    const picker = page.getByTestId("bar-recording-destination");
    await picker.selectOption("f-weekly");

    await page.evaluate(() => {
      const target = window as unknown as {
        __barTreeFails: boolean;
        __demoEmit: (event: string, payload: unknown) => void;
      };
      target.__barTreeFails = true;
      target.__demoEmit("murmur://reminder-visibility-invalidated", null);
    });

    await expect(picker).toHaveValue("");
    await expect(picker.locator("option[value='f-weekly']")).toHaveCount(0);

    // A later authorized reload can make the same opaque id visible again
    // without remounting/refocusing this persistent bar WebView. The prior
    // private choice must stay forgotten rather than silently re-selecting by id.
    await page.evaluate(() => {
      const target = window as unknown as {
        __barTreeFails: boolean;
        __demoEmit: (event: string, payload: unknown) => void;
      };
      target.__barTreeFails = false;
      target.__demoEmit("murmur://ask-history-invalidated", null);
    });
    await expect(picker.locator("option[value='f-weekly']")).toHaveCount(1);
    await expect(picker).toHaveValue("");
    await page.getByRole("button", { name: "Start recording" }).click();
    expect(await starts(page)).toEqual([{ folderId: null }]);
    expect(failures).toEqual({ consoleErrors: [], pageErrors: [] });
  });

  test("native refocus resets Unfiled and refreshes a persistent webview's forest", async ({
    page,
  }) => {
    const failures = await openBar(page);
    const picker = page.getByTestId("bar-recording-destination");
    await picker.selectOption("f-weekly");
    await expect(picker).toHaveValue("f-weekly");
    await expect
      .poll(() =>
        page.evaluate(() =>
          (
            window as unknown as {
              __demoEventListenerRegistrationCount: (event: string) => number;
            }
          ).__demoEventListenerRegistrationCount("tauri://focus"),
        ),
      )
      .toBe(1);

    await page.evaluate((forest) => {
      const target = window as unknown as {
        __barForest: unknown[];
        __demoEmit: (event: string, payload: unknown) => void;
      };
      target.__barForest = forest;
      // Rust's global toggle hides the persistent window, then shows it and
      // calls `set_focus()`. These are the two native events observed by the API.
      target.__demoEmit("tauri://blur", null);
      target.__demoEmit("tauri://focus", null);
    }, REOPENED_FOREST);

    await expect(picker).toHaveValue("");
    await expect(picker.locator("option[value='f-weekly']")).toContainText(
      "Folder · Roadmap / Planning",
    );
    await expect(picker.locator("option[value='f-fresh']")).toContainText(
      "Folder · Acme HQ / Fresh",
    );
    await expect(picker.locator("option[value='f-sealed']")).toHaveCount(0);
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __barTreeReads: number }).__barTreeReads,
        ),
      )
      .toBeGreaterThanOrEqual(2);
    expect(failures).toEqual({ consoleErrors: [], pageErrors: [] });
  });
});
