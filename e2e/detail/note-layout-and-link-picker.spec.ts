import { expect, test, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

const consoleErrors = new WeakMap<Page, string[]>();

test.beforeEach(({ page }) => {
  const errors: string[] = [];
  consoleErrors.set(page, errors);
  page.on("console", (message) => {
    if (message.type() === "error") {
      errors.push(message.text());
    }
  });
  page.on("pageerror", (error) => errors.push(String(error)));
});

test.afterEach(({ page }) => {
  expect(consoleErrors.get(page) ?? []).toEqual([]);
});

async function mockMeetingNote(page: Page): Promise<void> {
  await mockTauri(page, {
    list_links: () => [
      {
        id: 501,
        direction: "out",
        otherKind: "note",
        otherId: "n-related-one",
        otherTitle: "Launch checklist",
        edgeType: "manual",
        createdBy: "user",
        status: "active",
        score: 1,
        createdAt: 1_720_000_100,
        manual: true,
      },
      {
        id: 502,
        direction: "out",
        otherKind: "meeting",
        otherId: "m-related-two",
        otherTitle: "Roadmap follow-up",
        edgeType: "companion",
        createdBy: "auto",
        status: "active",
        score: 1,
        createdAt: 1_720_000_000,
        manual: false,
      },
    ],
    get_backlinks: () => [],
    get_related_picker_bootstrap: () => ({
      spaces: [
        {
          id: "p-scroll",
          name: "Performance Space",
          level: "project",
          emoji: null,
          locked: false,
          unlocked: false,
          linkable: true,
          groups: [{ kind: "meeting", total: 30 }],
          folders: [],
        },
      ],
      unclassified: [],
      anchor: {
        kind: "meeting",
        containerId: "p-scroll",
        path: ["p-scroll"],
        index: 15,
        // Production centres a bounded 24-item window on the anchor:
        // max(0, index - 24 / 2) = 3 for index 15.
        offset: 3,
        items: Array.from({ length: 24 }, (_, windowIndex) => {
          const index = windowIndex + 3;
          return index === 15
            ? {
                kind: "meeting",
                id: "m-atlas-roadmap",
                title: "Q2 Roadmap Planning",
              }
            : {
                kind: "meeting",
                id: `scroll-candidate-${index}`,
                title: `Scroll candidate ${String(index).padStart(2, "0")}`,
              };
        }),
        total: 30,
      },
    }),
    list_shared_workspace: () => ({
      spaces: [],
      sharedBrains: {
        orgId: "shared",
        orgName: "Shared",
        name: "Shared Brains",
        level: "virtual",
        access: "view",
        authorHint: "",
        folders: [],
        items: [],
        position: 0,
      },
    }),
    list_builtin_recipes: (args: unknown) => {
      const w = window as unknown as { __recipeReads?: unknown[] };
      (w.__recipeReads ??= []).push({ command: "list_builtin_recipes", args });
      return [];
    },
    list_saved_recipes: (args: unknown) => {
      const w = window as unknown as { __recipeReads?: unknown[] };
      (w.__recipeReads ??= []).push({ command: "list_saved_recipes", args });
      return [];
    },
  });
}

async function openRelatedPicker(page: Page) {
  await mockMeetingNote(page);
  await page.goto("/meeting/m-atlas-roadmap");

  const panel = page.locator("app-note-panel app-connections");
  await expect(panel).toBeVisible({ timeout: 10_000 });

  const collapsed = panel.getByRole("button", {
    name: "Show related items and suggestions",
  });
  if ((await collapsed.count()) > 0) {
    await collapsed.click();
  }

  const trigger = panel.getByRole("button", { name: "Link", exact: true });
  await expect(trigger).toBeVisible();
  await trigger.click();

  const picker = page.getByRole("dialog", { name: "Add related" });
  const search = picker.getByPlaceholder("Search every Space…");
  const tree = picker.locator(".rhp-tree");
  await expect(picker).toBeVisible();
  await expect(search).toBeVisible();
  await expect(tree.locator('[data-row^="i:meeting:"]')).toHaveCount(24);
  await expect(
    tree.getByRole("treeitem", { name: "Load earlier" }),
  ).toBeVisible();
  await expect(tree.getByRole("treeitem", { name: "Load more" })).toBeVisible();
  return { picker, search, tree };
}

async function waitForPickerMotion(page: Page): Promise<void> {
  await page
    .getByRole("dialog", { name: "Add related" })
    .evaluate(async (element) => {
      const animations = new Set<Animation>();
      for (
        let current: Element | null = element;
        current;
        current = current.parentElement
      ) {
        for (const animation of current.getAnimations()) {
          animations.add(animation);
        }
      }
      await Promise.all(
        [...animations].map((animation) =>
          animation.finished.catch(() => undefined),
        ),
      );
      await new Promise<void>((resolve) =>
        requestAnimationFrame(() =>
          requestAnimationFrame(() => resolve()),
        ),
      );
    });
}

test("meeting Note view omits Generate without loading recipe catalogs", async ({
  page,
}) => {
  await mockMeetingNote(page);
  await page.goto("/meeting/m-atlas-roadmap");

  const notePanel = page.locator("app-note-panel");
  await expect(notePanel).toBeVisible({ timeout: 10_000 });
  await expect(notePanel.locator("app-meeting-recipes")).toHaveCount(0);
  await expect(
    notePanel.getByRole("heading", { name: "Generate" }),
  ).toHaveCount(0);

  const recipeReads = await page.evaluate(
    () =>
      (window as unknown as { __recipeReads?: unknown[] }).__recipeReads ?? [],
  );
  expect(recipeReads).toEqual([]);
});

test("meeting Related is the first, prominent, expanded Note section", async ({
  page,
}) => {
  await mockMeetingNote(page);
  await page.goto("/meeting/m-atlas-roadmap");

  const notePanel = page.locator("app-note-panel .note-panel");
  const related = notePanel.locator(":scope > .related-primary");
  await expect(related).toBeVisible({ timeout: 10_000 });
  expect(
    await related.evaluate(
      (element) => element.previousElementSibling === null,
    ),
  ).toBe(true);

  const connections = related.locator("app-connections");
  await expect(connections.locator(".cx--prominent")).toBeVisible();
  await expect(
    connections.getByRole("button", { name: "Hide related items" }),
  ).toHaveAttribute("aria-expanded", "true");
  await expect(
    connections.getByRole("button", { name: "Link", exact: true }),
  ).toBeVisible();
  await expect(
    connections.getByText("Launch checklist", { exact: true }),
  ).toBeVisible();
});

test("Related hierarchy picker fits in the default 900x680 meeting window", async ({
  page,
}) => {
  await page.setViewportSize({ width: 900, height: 680 });
  const { picker, search, tree } = await openRelatedPicker(page);
  await waitForPickerMotion(page);

  const [pickerBox, searchBox, treeBox] = await Promise.all([
    picker.boundingBox(),
    search.boundingBox(),
    tree.boundingBox(),
  ]);
  expect(pickerBox).not.toBeNull();
  expect(searchBox).not.toBeNull();
  expect(treeBox).not.toBeNull();
  expect(pickerBox!.x).toBeGreaterThanOrEqual(8);
  expect(pickerBox!.y).toBeGreaterThanOrEqual(8);
  expect(pickerBox!.x + pickerBox!.width).toBeLessThanOrEqual(892);
  expect(pickerBox!.y + pickerBox!.height).toBeLessThanOrEqual(672);
  expect(searchBox!.y).toBeGreaterThanOrEqual(pickerBox!.y);
  expect(treeBox!.y).toBeGreaterThan(searchBox!.y + searchBox!.height);
  expect(treeBox!.y + treeBox!.height).toBeLessThanOrEqual(
    pickerBox!.y + pickerBox!.height,
  );
});

test("fast scrolling inside Related picker does not re-layout or blank it", async ({
  page,
}) => {
  const { picker, tree } = await openRelatedPicker(page);
  await waitForPickerMotion(page);
  await expect(tree).toHaveCSS("overscroll-behavior", "contain");

  const measurements = await tree.evaluate(async (element) => {
    const descriptor = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "offsetHeight",
    );
    if (!descriptor?.get || !descriptor.configurable) {
      return { supported: false, reads: -1 };
    }

    let reads = 0;
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
      configurable: true,
      get() {
        if (this === element) {
          reads += 1;
        }
        return descriptor.get!.call(this);
      },
    });
    try {
      for (let index = 0; index < 20; index += 1) {
        element.scrollTop = index % 2 === 0 ? element.scrollHeight : 0;
        await new Promise<void>((resolve) =>
          requestAnimationFrame(() => resolve()),
        );
      }
      element.scrollTop = element.scrollHeight;
      await new Promise<void>((resolve) =>
        requestAnimationFrame(() =>
          requestAnimationFrame(() => resolve()),
        ),
      );
      return { supported: true, reads };
    } finally {
      Object.defineProperty(
        HTMLElement.prototype,
        "offsetHeight",
        descriptor,
      );
    }
  });

  expect(measurements.supported).toBe(true);
  expect(measurements.reads).toBe(0);
  await expect(tree.locator('[data-row^="i:meeting:"]')).toHaveCount(24);
  await expect(tree.locator(".rhp-state")).toHaveCount(0);
  await expect(
    picker.getByRole("treeitem", { name: /Scroll candidate 26/ }),
  ).toBeVisible();
});
