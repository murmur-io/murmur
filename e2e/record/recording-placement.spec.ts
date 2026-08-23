import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * A finished recording can be filed into a project or folder, and a SEALED
 * destination cannot receive it.
 *
 * A meeting is the one kind a user cannot create into a container — it creates
 * itself when recording stops — so the placement decision has to happen after
 * the fact. This pins two things the UI cannot get wrong:
 *
 *  1. Picking a destination calls `move_note` with THAT container id. The mock
 *     records the real argument in the page, so a card that renders a list but
 *     files somewhere else (or nowhere) fails rather than merely looking right.
 *  2. A sealed-and-not-session-unlocked container is not clickable. The backend
 *     refuses that move with `AppError::Locked` and is right to — there is no
 *     content key to seal the arriving meeting with — but a destination a click
 *     cannot reach must not look reachable.
 *
 * RED contract: drop the `[disabled]` binding and the locked row takes the
 * click, so `move_note` is invoked for `f-sealed` and the last assertion fails;
 * make `file()` ignore its argument and the first one fails.
 */
test.describe("Record — filing a finished recording", () => {
  const container = (
    id: string,
    name: string,
    level: "project" | "folder",
    locked: boolean,
    folders: unknown[] = [],
  ) => ({
    id,
    name,
    level,
    emoji: null,
    tint: null,
    locked,
    unlocked: false,
    isRoot: false,
    folders,
    groups: [],
  });

  const forest = [
    container("p-acme", "Acme", "project", false, [
      container("f-weekly", "Weekly", "folder", false),
      container("f-sealed", "Board", "folder", true),
    ]),
  ];

  test("picking a destination files the meeting there, and a sealed one is refused", async ({
    page,
  }) => {
    await mockTauri(
      page,
      {
        // The override is stringified into the page, so the recorder lives on
        // `window` — a closure over a Node-side array would never be written to.
        move_note: (args: unknown) => {
          const w = window as unknown as { __moves?: unknown[] };
          (w.__moves ??= []).push(args);
          return null;
        },
      },
      {
        model_present: true,
        start_recording: {
          meetingId: "m-rec",
          startedAt: "2026-07-01T09:00:00Z",
        },
        stop_recording: {
          meetingId: "m-rec",
          markdown: "# Notes",
          exportedPath: "/vault/Notes/m-rec.md",
        },
        list_workspace_tree: forest,
      },
    );

    await page.goto("/record");
    await page.locator("button.start-btn").click();
    await page.locator("button.stop-btn").click();

    const card = page.locator("[data-testid='recording-placement']");
    await expect(card).toBeVisible({ timeout: 10_000 });

    // The sealed folder is offered but not reachable.
    const sealed = page.locator("[data-testid='placement-destination-f-sealed']");
    await expect(sealed).toBeVisible();
    await expect(sealed).toBeDisabled();

    // The open folder files the meeting — and the call carries ITS id.
    await page.locator("[data-testid='placement-destination-f-weekly']").click();
    await expect(page.locator("[data-testid='placement-filed']")).toContainText(
      "Weekly",
    );

    const moves = await page.evaluate(
      () => (window as unknown as { __moves?: unknown[] }).__moves ?? [],
    );
    expect(moves).toEqual([{ meetingId: "m-rec", folderId: "f-weekly" }]);
  });
});
