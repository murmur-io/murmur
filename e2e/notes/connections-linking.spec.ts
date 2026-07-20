import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";

/**
 * PR-1 — user-initiated linking UI in the shared `app-connections` panel (mounted
 * in the note editor at /notes/n1). Verifies, against a mocked Tauri IPC:
 *  - `list_links` renders a mix of chips; ONLY the `manual:true` chip shows a `×`.
 *  - clicking that `×` invokes `unlink_items` with the anchor→neighbour args and
 *    then re-fetches `list_links` (the chip drops out).
 *  - the `+ Link` chooser opens the OPAQUE single-pick popover (`app-link-picker`),
 *    and picking a candidate invokes `link_items(anchorKind, anchorId, kind, id)`
 *    then re-fetches (the new chip appears).
 *
 * The mock records every link_items/unlink_items call on `window.__linkCalls` so the
 * test can assert the exact args (the overrides are serialized page-side — no test
 * closures — so they stash calls on `window` and drive `list_links` off a flag they
 * also set on `window`). The core gate is ZERO console/page errors through the flow.
 */
test("connections panel: × only on manual chips (unlink), + Link chooser links a candidate — no console errors", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, {
    // list_links: three chips before any mutation — a MANUAL (removable) wikilink,
    // an AUTO companion (not removable), and a SEMANTIC suggestion. After an unlink
    // or a link the mock flips window.__linksAfter so the re-fetch returns a fresh set.
    list_links: () => {
      const w = window as unknown as {
        __linksAfterUnlink?: boolean;
        __linksAfterLink?: boolean;
      };
      const rows = [
        // The manual (removable) chip — dropped once __linksAfterUnlink flips.
        {
          id: 1,
          direction: "out",
          otherKind: "meeting",
          otherId: "m-manual",
          otherTitle: "Manual Meeting Link",
          edgeType: "wikilink",
          createdBy: "user",
          status: "active",
          score: 1.0,
          createdAt: 1_720_000_100,
          manual: true,
        },
        {
          id: 2,
          direction: "out",
          otherKind: "note",
          otherId: "n2",
          otherTitle: "Weekly plan",
          edgeType: "companion",
          createdBy: "auto",
          status: "active",
          score: 1.0,
          createdAt: 1_720_000_000,
          manual: false,
        },
        {
          id: 3,
          direction: "in",
          otherKind: "note",
          otherId: "n-sugg",
          otherTitle: "A suggested note",
          edgeType: "semantic",
          createdBy: "auto",
          status: "suggested",
          score: 0.86,
          createdAt: 1_720_000_050,
          manual: false,
        },
      ];
      const out = w.__linksAfterUnlink
        ? rows.filter((r) => r.id !== 1) // manual chip removed by unlink
        : rows;
      if (w.__linksAfterLink) {
        out.push({
          id: 9,
          direction: "out",
          otherKind: "note",
          otherId: "n-picked",
          otherTitle: "Picked Note",
          edgeType: "manual",
          createdBy: "user",
          status: "active",
          score: 1.0,
          createdAt: 1_720_000_200,
          manual: true,
        });
      }
      return out;
    },

    // The single-pick chooser's candidate feed: a note + a meeting + an org row
    // (org must be filtered out of the chooser as a non-linkable endpoint).
    list_link_candidates: () => [
      { kind: "note", id: "n-picked", title: "Picked Note", snippet: "" },
      { kind: "meeting", id: "m-cand", title: "Some meeting", snippet: "" },
      { kind: "org", id: "org-1", title: "Org item", snippet: "" },
    ],

    // Record link/unlink calls + flip the re-fetch flag so list_links changes.
    link_items: (args: unknown) => {
      const w = window as unknown as {
        __linkCalls?: unknown[];
        __linksAfterLink?: boolean;
      };
      (w.__linkCalls = w.__linkCalls || []).push({ cmd: "link_items", args });
      w.__linksAfterLink = true;
      return null;
    },
    unlink_items: (args: unknown) => {
      const w = window as unknown as {
        __linkCalls?: unknown[];
        __linksAfterUnlink?: boolean;
      };
      (w.__linkCalls = w.__linkCalls || []).push({ cmd: "unlink_items", args });
      w.__linksAfterUnlink = true;
      return null;
    },

    // Keep backlinks empty so only the connections panel is under test.
    get_backlinks: () => [],
  });

  await page.goto("/notes/n1");

  const panel = page.locator("app-connections");
  await expect(panel).toBeVisible();

  // Existing relationships stay collapsed by default, but linking is an
  // independent command: it must remain reachable without revealing the rows.
  const collapsed = panel.getByRole("button", {
    name: "Show related items and suggestions",
  });
  await expect(collapsed).toHaveAttribute("aria-expanded", "false");
  const collapsedLink = panel.getByRole("button", {
    name: "Link",
    exact: true,
  });
  await expect(collapsedLink).toBeVisible();
  await collapsedLink.click();
  await expect(panel.getByPlaceholder(/Link a meeting, note/)).toBeVisible();
  await expect(
    page.locator(".link-pop").getByRole("option", { name: /Picked Note/ }),
  ).toBeVisible();
  await expect(collapsed).toHaveAttribute("aria-expanded", "false");
  await expect(panel.locator(".cx-group")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(collapsedLink).toBeVisible();

  // Expanding is only for inspecting/managing the relationship rows.
  await collapsed.click();

  // Three chips render in the merged "Related" panel: the manual (removable) link,
  // the auto companion link, and the semantic suggestion (an ambient dashed chip).
  await expect(panel.getByText("Manual Meeting Link")).toBeVisible();
  await expect(panel.getByText("Weekly plan")).toBeVisible();
  await expect(panel.getByText("A suggested note")).toBeVisible();

  // The MANUAL deterministic chip carries a "Remove link to …" × (unlink); the
  // semantic suggestion carries a "Dismiss suggestion …" × instead (2026-07-19 IA:
  // suggestions are ambient dashed chips — tap promotes, hover × dismisses — no
  // persistent Accept/Dismiss buttons). Target by aria-label, not a bare .cx-remove
  // count (both kinds now use the same hover-× visual).
  const unlinkBtn = panel.getByRole("button", {
    name: "Remove link to Manual Meeting Link",
  });
  await expect(unlinkBtn).toBeVisible();

  // Click the × → unlink_items(anchorKind=note, anchorId=n1, otherKind=meeting,
  // otherId=m-manual) is invoked, then the re-fetch drops the manual chip.
  await unlinkBtn.click();
  await expect(panel.getByText("Manual Meeting Link")).toHaveCount(0);

  const afterUnlink = await page.evaluate(
    () => (window as unknown as { __linkCalls?: unknown[] }).__linkCalls || [],
  );
  expect(afterUnlink).toEqual([
    {
      cmd: "unlink_items",
      args: {
        srcKind: "note",
        srcId: "n1",
        dstKind: "meeting",
        dstId: "m-manual",
      },
    },
  ]);

  // Open the + Link chooser → the query input + the opaque picker popover appear.
  // `exact` so the substring name doesn't also match a suggestion chip's
  // "Add link to …" aria-label (2026-07-19 ambient suggestion chips are buttons).
  await panel.getByRole("button", { name: "Link", exact: true }).click();
  await expect(panel.getByPlaceholder(/Link a meeting, note/)).toBeVisible();
  // The link-picker popover box is TELEPORTED to <body> (appTeleportToBody) —
  // locate it by class, not by the `app-link-picker` host.
  const picker = page.locator(".link-pop");
  await expect(picker).toBeVisible();

  // The picker offers the candidates (org row is not a valid endpoint; picking a
  // note candidate invokes link_items(anchor=note/n1 → note/n-picked)).
  const pickedRow = picker.getByRole("option", { name: /Picked Note/ });
  await expect(pickedRow).toBeVisible();
  // mousedown is prevented on the popover (no focus steal), then click emits picked.
  await pickedRow.dispatchEvent("click");

  // The new manual chip appears from the re-fetch; link_items was called correctly.
  await expect(panel.getByText("Picked Note")).toBeVisible();
  const afterLink = await page.evaluate(
    () => (window as unknown as { __linkCalls?: unknown[] }).__linkCalls || [],
  );
  expect(afterLink).toContainEqual({
    cmd: "link_items",
    args: { srcKind: "note", srcId: "n1", dstKind: "note", dstId: "n-picked" },
  });

  expect(consoleErrors).toEqual([]);
});

/**
 * Regression for the empty-state trap: the panel used to be gated by
 * `hasAnything()`, which hid its only manual-link entry point precisely when a
 * note had no relationships. This drives the real empty DOM through the shared
 * picker and proves that creating the first link transitions to `Related · 1`.
 */
test("empty Related state keeps + Link reachable and creates the first relationship", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      consoleErrors.push(msg.text());
    }
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockNotes(page, {
    list_links: () => {
      const linked = (window as unknown as { __emptyLinked?: boolean })
        .__emptyLinked;
      return linked
        ? [
            {
              id: 91,
              direction: "out",
              otherKind: "note",
              otherId: "n-first",
              otherTitle: "First linked note",
              edgeType: "manual",
              createdBy: "user",
              status: "active",
              score: 1.0,
              createdAt: 1_720_000_300,
              manual: true,
            },
          ]
        : [];
    },
    get_backlinks: () => [],
    list_link_candidates: (args: unknown) => {
      const w = window as unknown as {
        __emptyCandidateCalls?: unknown[];
      };
      (w.__emptyCandidateCalls ??= []).push(args);
      return [
        // The picker must exclude its own anchor while retaining a valid target.
        { kind: "note", id: "n1", title: "My First Note", snippet: "" },
        {
          kind: "note",
          id: "n-first",
          title: "First linked note",
          snippet: "",
        },
      ];
    },
    link_items: (args: unknown) => {
      const w = window as unknown as {
        __emptyLinkCalls?: unknown[];
        __emptyLinked?: boolean;
      };
      (w.__emptyLinkCalls ??= []).push(args);
      w.__emptyLinked = true;
      return null;
    },
  });

  await page.goto("/notes/n1");

  const panel = page.locator("app-connections");
  await expect(panel).toBeVisible();
  await expect(panel.locator(".cx--empty")).toBeVisible();
  await expect(panel.locator(".cx-collapsed")).toHaveCount(0);

  const linkTrigger = panel.getByRole("button", { name: "Link", exact: true });
  await expect(linkTrigger).toBeVisible();
  await linkTrigger.click();

  await expect(panel.getByPlaceholder(/Link a meeting, note/)).toBeVisible();
  const picker = page.locator(".link-pop");
  await expect(picker).toBeVisible();
  await expect(
    picker.getByRole("option", { name: /My First Note/ }),
  ).toHaveCount(0);

  const firstTarget = picker.getByRole("option", {
    name: /First linked note/,
  });
  await expect(firstTarget).toBeVisible();
  await firstTarget.dispatchEvent("click");

  // The write re-fetches server truth: the empty trigger becomes the normal
  // collapsed Related row, and expanding it reveals the newly linked chip.
  const collapsed = panel.getByRole("button", {
    name: "Show related items and suggestions",
  });
  await expect(collapsed).toBeVisible();
  await expect(collapsed.locator(".cx-count")).toHaveText("1");
  await expect(
    panel.getByRole("button", { name: "Link", exact: true }),
  ).toBeVisible();
  await collapsed.click();
  await expect(panel.getByText("First linked note", { exact: true })).toBeVisible();

  const linkCalls = await page.evaluate(
    () =>
      (window as unknown as { __emptyLinkCalls?: unknown[] })
        .__emptyLinkCalls ?? [],
  );
  expect(linkCalls).toEqual([
    {
      srcKind: "note",
      srcId: "n1",
      dstKind: "note",
      dstId: "n-first",
    },
  ]);

  const candidateCalls = await page.evaluate(
    () =>
      (window as unknown as { __emptyCandidateCalls?: unknown[] })
        .__emptyCandidateCalls ?? [],
  );
  expect(candidateCalls).toContainEqual({ prefix: "", offset: 0, limit: 40 });
  expect(consoleErrors).toEqual([]);
});

/**
 * Lock negative: a masked note must not mount relationship UI or make any
 * relationship/candidate read. This pins the note-editor gate in addition to the
 * component's own `@if (!locked())` belt-and-braces guard.
 */
test("locked note hides Related and performs no relationship picker reads", async ({
  page,
}) => {
  await mockNotes(page, {
    list_links: (args: unknown) => {
      const w = window as unknown as { __lockedConnectionReads?: unknown[] };
      (w.__lockedConnectionReads ??= []).push({ cmd: "list_links", args });
      return [];
    },
    get_backlinks: (args: unknown) => {
      const w = window as unknown as { __lockedConnectionReads?: unknown[] };
      (w.__lockedConnectionReads ??= []).push({ cmd: "get_backlinks", args });
      return [];
    },
    list_link_candidates: (args: unknown) => {
      const w = window as unknown as { __lockedConnectionReads?: unknown[] };
      (w.__lockedConnectionReads ??= []).push({
        cmd: "list_link_candidates",
        args,
      });
      return [];
    },
  });

  await page.goto("/notes/nlk");

  await expect(page.getByText("This note is locked")).toBeVisible();
  await expect(page.locator("app-connections")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Link", exact: true }),
  ).toHaveCount(0);
  await expect(page.locator(".link-pop")).toHaveCount(0);

  const reads = await page.evaluate(
    () =>
      (window as unknown as { __lockedConnectionReads?: unknown[] })
        .__lockedConnectionReads ?? [],
  );
  expect(reads).toEqual([]);
});

/**
 * WKWebView regression: a teleported fixed picker used to re-measure/write its
 * layer once for every captured scroll event. A fast scroll of the note pane
 * queued dozens of forced layouts and WebKit temporarily painted the opaque
 * picker shell with none of its already-mounted rows.
 */
test("fast note-pane scrolling coalesces picker layout and keeps its rows painted", async ({
  page,
}) => {
  await mockNotes(page, {
    get_note: (args: { id: string }) => ({
      id: args.id,
      title: "Long standalone note",
      folderId: "nf1",
      markdown: Array.from(
        { length: 180 },
        (_, index) => `Paragraph ${index}: enough text to make the note pane scroll.`,
      ).join("\n\n"),
      tags: [],
      properties: {},
      updatedAt: 1_720_000_000_000,
      createdAt: 1_719_000_000_000,
      exportedPath: null,
      locked: false,
      shared: false,
    }),
    list_links: () => [
      {
        id: 601,
        direction: "out",
        otherKind: "meeting",
        otherId: "m-existing",
        otherTitle: "Existing meeting",
        edgeType: "manual",
        createdBy: "user",
        status: "active",
        score: 1,
        createdAt: 1_720_000_000,
        manual: true,
      },
    ],
    get_backlinks: () => [],
    list_link_candidates: () =>
      Array.from({ length: 30 }, (_, index) => ({
        kind: "note",
        id: `page-scroll-candidate-${index}`,
        title: `Page scroll candidate ${String(index).padStart(2, "0")}`,
        snippet: "",
      })),
  });

  await page.goto("/notes/n1");

  const panel = page.locator("app-connections");
  await expect(panel).toBeVisible();
  const collapsed = panel.getByRole("button", {
    name: "Show related items and suggestions",
  });
  await collapsed.click();
  await panel.getByRole("button", { name: "Link", exact: true }).click();

  const input = panel.getByPlaceholder(/Link a meeting, note/);
  const picker = page.locator(".link-pop");
  await expect(input).toBeVisible();
  await expect(picker.locator(".link-pop-row")).toHaveCount(30);

  const measurements = await input.evaluate(async (element) => {
    const inputElement = element as HTMLInputElement & {
      getBoundingClientRect: () => DOMRect;
    };
    const originalRect = inputElement.getBoundingClientRect.bind(inputElement);
    const heightDescriptor = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "offsetHeight",
    );
    const pickerElement = document.querySelector<HTMLElement>(".link-pop");
    if (!heightDescriptor?.get || !heightDescriptor.configurable || !pickerElement) {
      return { supported: false, anchorReads: -1, pickerHeightReads: -1 };
    }

    let anchorReads = 0;
    let pickerHeightReads = 0;
    inputElement.getBoundingClientRect = () => {
      anchorReads += 1;
      return originalRect();
    };
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
      configurable: true,
      get() {
        if (this === pickerElement) {
          pickerHeightReads += 1;
        }
        return heightDescriptor.get!.call(this);
      },
    });

    try {
      const scroller = document.querySelector<HTMLElement>(".editor-body");
      if (!scroller || scroller.scrollHeight <= scroller.clientHeight) {
        return { supported: false, anchorReads: -1, pickerHeightReads: -1 };
      }
      const max = scroller.scrollHeight - scroller.clientHeight;
      const travel = Math.min(max, 160);
      for (let index = 0; index < 24; index += 1) {
        scroller.scrollTop = index % 2 === 0 ? travel : 0;
        scroller.dispatchEvent(new Event("scroll"));
        await new Promise<void>((resolve) =>
          requestAnimationFrame(() => resolve()),
        );
      }
      await new Promise<void>((resolve) =>
        requestAnimationFrame(() => resolve()),
      );
      return { supported: true, anchorReads, pickerHeightReads };
    } finally {
      inputElement.getBoundingClientRect = originalRect;
      Object.defineProperty(
        HTMLElement.prototype,
        "offsetHeight",
        heightDescriptor,
      );
    }
  });

  expect(measurements.supported).toBe(true);
  // One live-anchor measurement per animation frame is required to follow the
  // input, but the already-fitted picker must not synchronously remeasure its
  // own layout while the scroll compositor is moving it.
  expect(measurements.anchorReads).toBeLessThanOrEqual(26);
  expect(measurements.pickerHeightReads).toBeLessThanOrEqual(2);
  await expect(picker.locator(".link-pop-row")).toHaveCount(30);
  await expect(picker.locator(".link-pop-empty")).toHaveCount(0);
  await expect(
    picker.getByRole("option", { name: /Page scroll candidate 29/ }),
  ).toBeVisible();
});
