import { expect, test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { mockTauri } from "../settings-ai/mock-invoke";

const FOREST = [
  {
    id: "p-acme",
    name: "Acme",
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [
      {
        kind: "meeting",
        total: 1,
        items: [
          { kind: "meeting", id: "m-1", title: "Standup", durationS: 900, sortAt: 2 },
        ],
      },
      {
        kind: "task",
        total: 1,
        items: [
          { kind: "task", id: "t-1", title: "Ship the thing", durationS: null, sortAt: 1 },
        ],
      },
      {
        kind: "dashboard",
        total: 1,
        items: [
          { kind: "dashboard", id: "d-1", title: "Q3 board", durationS: null, sortAt: 0 },
        ],
      },
    ],
  },
  {
    id: "p-target",
    name: "Target",
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
  {
    id: "p-sealed",
    name: "Clients",
    kind: "meeting",
    level: "project",
    emoji: null,
    tint: null,
    locked: true,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: [],
  },
];

async function open(page: Page): Promise<void> {
  // Tall enough that the whole tree fits: a rail that scrolls mid-drag moves the source
  // out from under the pointer, and the drag never completes.
  await page.setViewportSize({ width: 1280, height: 1400 });
  await mockTauri(
    page,
    {
      move_note: (args: unknown) => {
        const w = globalThis as unknown as { __moves?: unknown[] };
        (w.__moves ??= []).push({ cmd: "move_note", args });
        return null;
      },
      move_note_doc: (args: unknown) => {
        const w = globalThis as unknown as { __moves?: unknown[] };
        (w.__moves ??= []).push({ cmd: "move_note_doc", args });
        return null;
      },
      move_dashboard_to_container: (args: unknown) => {
        const w = globalThis as unknown as { __moves?: unknown[] };
        (w.__moves ??= []).push({ cmd: "move_dashboard_to_container", args });
        return null;
      },
      set_task_container: (args: unknown) => {
        const w = globalThis as unknown as { __moves?: unknown[] };
        (w.__moves ??= []).push({ cmd: "set_task_container", args });
        return null;
      },
    },
    { list_workspace_tree: FOREST },
  );
  await page.goto("/");
  await expect(page.getByRole("tree", { name: "Workspaces" })).toBeVisible();
  await page.getByRole("button", { name: "Expand Acme" }).click();
}

test("every kind the tree renders is draggable", async ({ page }) => {
  await open(page);

  // This test used to assert the OPPOSITE for tasks and dashboards, and was right to: neither
  // had a container anchor, so a drag would have been a gesture with nothing behind it. Both
  // gained a backend mover, and a row a user can see under a project is a row they will try to
  // drag out of it.
  await expect(page.getByRole("treeitem", { name: /Standup/ })).toHaveAttribute(
    "draggable",
    "true",
  );

  await expect(page.getByRole("treeitem", { name: /Ship the thing/ })).toHaveAttribute(
    "draggable",
    "true",
  );

  await expect(page.getByRole("treeitem", { name: /Q3 board/ })).toHaveAttribute(
    "draggable",
    "true",
  );
});

test("each kind is filed through its OWN backend mover", async ({ page }) => {
  await open(page);

  // The four kinds do NOT share a command, and the id alone cannot say which one to call — a
  // single mover for all of them would file a board through the note path and lose it. Each
  // drag is checked for the command AND its argument shape, against the real invoke path.
  await page
    .getByRole("treeitem", { name: /Standup/ })
    .dragTo(page.getByRole("treeitem", { name: /Target/ }));

  await page
    .getByRole("treeitem", { name: /Q3 board/ })
    .dragTo(page.getByRole("treeitem", { name: /Target/ }));

  await page
    .getByRole("treeitem", { name: /Ship the thing/ })
    .dragTo(page.getByRole("treeitem", { name: /Target/ }));

  const moves = await page.evaluate(
    () => (globalThis as unknown as { __moves?: unknown[] }).__moves ?? [],
  );
  expect(moves).toEqual([
    { cmd: "move_note", args: { meetingId: "m-1", folderId: "p-target" } },
    {
      cmd: "move_dashboard_to_container",
      args: { id: "d-1", folderId: "p-target" },
    },
    { cmd: "set_task_container", args: { id: "t-1", containerId: "p-target" } },
  ]);
});

test("dropping a meeting on a container files it there", async ({ page }) => {
  await open(page);

  // Grab the row by its BODY, the way a user does. The treeitem's centre can fall on the
  // trailing control, which is not the drag handle — and a drag that never starts looks
  // exactly like a drop that was refused.
  await page
    .getByRole("treeitem", { name: /Standup/ })
    .dragTo(page.getByRole("treeitem", { name: /Target/ }));

  const moves = await page.evaluate(
    () => (globalThis as unknown as { __moves?: unknown[] }).__moves ?? [],
  );
  expect(moves).toEqual([
    { cmd: "move_note", args: { meetingId: "m-1", folderId: "p-target" } },
  ]);
});

test("a sealed container is not a drop target", async ({ page }) => {
  await open(page);

  await page
    .getByRole("treeitem", { name: /Standup/ })
    .dragTo(page.getByRole("treeitem", { name: /Clients/ }));

  // Every mover refuses a sealed, not-unlocked destination, so arming it would only
  // invite a drop that can fail.
  const moves = await page.evaluate(
    () => (globalThis as unknown as { __moves?: unknown[] }).__moves ?? [],
  );
  expect(moves).toEqual([]);
});

/**
 * The half of drag-and-drop this suite is structurally blind to.
 *
 * # What shipped broken
 *
 * Every test above passed — in Chromium AND WebKit — while dragging a row in the shipped app did
 * nothing at all. The row picked up and followed the cursor, no Workspace ever armed, no drop ever
 * landed. The tests were not lying: the DOM half is correct, and Playwright dispatches the drag
 * straight into the engine. The app has a native layer in front of the engine, and Playwright has
 * no way to put one there.
 *
 * # The mechanism
 *
 * wry overrides `draggingEntered:`, `draggingUpdated:` and `performDragOperation:` on its WKWebView
 * subclass (`wry/src/wkwebview/drag_drop.rs`). Each asks Tauri's drag-drop handler first and calls
 * `super` — WKWebView's own implementation, the only path into the web content — ONLY if that
 * handler declines. `tauri-runtime-wry` installs a handler that returns `true` unconditionally
 * whenever the window's `dragDropEnabled` is true, and `true` is the default (`tauri-utils`
 * `default_true`). So on a default config the native override answers every drag itself and the page
 * is never told one happened: no `dragenter`, no `dragover`, no `drop`.
 *
 * `dragstart` still fires, because that is the drag SOURCE half and nothing intercepts it. A gesture
 * that starts, follows the cursor and can never land is exactly what a user reports as "drag and
 * drop does not work" — and exactly what no green DOM test can see.
 *
 * # Why the guard is a config assertion and not a browser test
 *
 * The defect is not reachable from a browser: it lives in the window layer of the packaged app. What
 * IS checkable everywhere is the one line that keeps the native layer out of the way, so that is
 * what this asserts, for every window the config declares — a second window added later would
 * default straight back into the bug. The floating recorder bar is built in Rust
 * (`lib.rs::create_bar_window`) and is deliberately not covered: it is a 540x58 transport control
 * with nothing to drop onto. A future window that renders draggable content needs
 * `.drag_drop_handler_enabled(false)` on its builder, for the same reason.
 *
 * The control that keeps this honest is `scripts/wkwebview-drag-probe` (`--self-test`), which builds
 * both configurations in a real WKWebView and measures the asymmetry the way AppKit produces it:
 *
 *   dragDropEnabled true  → page saw {enter:0, over:0, drop:0}
 *   dragDropEnabled false → page saw {enter:1, over:1, drop:1}, operation = move
 *
 * It needs a WindowServer session, so it is not part of `scripts/ci.sh`; it is how this diagnosis was
 * made, and how to re-make it if the upstream behaviour ever changes.
 */
test("the native layer is kept out of the way of HTML5 drag-and-drop", async () => {
  const conf = JSON.parse(
    readFileSync(join(__dirname, "..", "..", "src-tauri", "tauri.conf.json"), "utf8"),
  ) as { app: { windows: { label?: string; title?: string; dragDropEnabled?: boolean }[] } };

  expect(conf.app.windows.length).toBeGreaterThan(0);
  for (const window of conf.app.windows) {
    expect(
      window.dragDropEnabled,
      `window ${window.label ?? window.title ?? "?"}: dragDropEnabled must be false, or wry's ` +
        "NSDraggingDestination override answers every drag and the page never sees one",
    ).toBe(false);
  }
});
