import { test, expect } from "@playwright/test";
import { mockNotes } from "./mock-invoke";
import { mockTauri } from "../settings-ai/mock-invoke";
import { taskStates, toggleTask } from "../../src/app/shared/markdown/task-list";
import { preprocessMarkdown } from "../../src/app/shared/markdown/preprocess";

/**
 * GFM task lists (`- [ ]` / `- [x]`). They used to render as plain bullets:
 * marked emits `<input type="checkbox">`, and Angular's `[innerHTML]` sanitizer
 * drops `<input>` entirely. The boxes are now `role="checkbox"` spans, and in a
 * note's Preview they are clickable and write the flipped marker back to the
 * markdown source.
 */

const NOTE_BODY = [
  "# Groceries",
  "",
  "- [ ] milk",
  "- [x] eggs",
  "  - [ ] free-range",
  "",
  "```",
  "- [ ] not a task (code)",
  "```",
  "",
  "1. [ ] ordered task",
].join("\n");

const MEETING_TODO = {
  locked: false,
  meeting: {
    id: "m-todo",
    startedAt: "2026-08-13T09:00:00Z",
    endedAt: "2026-08-13T10:00:00Z",
    title: "Planning",
    durationS: 3600,
    audioPath: null,
    status: "SUMMARIZED",
    folderId: null,
  },
  note: {
    meetingId: "m-todo",
    providerId: "claude_code",
    markdown: "# Planning\n\n## Action items\n- [ ] Send the deck\n- [ ] Book the room",
    exportedPath: null,
  },
  segments: [],
  assistantInteractions: [],
  aiProvider: "claude_code",
  aiModel: "gpt-5.6-codex",
  modelServed: "gpt-5.6-codex",
};

test("task list items render as checkboxes, not bare bullets", async ({ page }) => {
  await mockNotes(page, {
    get_note: (args: { id: string }) => ({
      id: args.id,
      title: "Todo",
      folderId: "nf1",
      markdown:
        "# Groceries\n\n- [ ] milk\n- [x] eggs\n  - [ ] free-range\n\n```\n- [ ] not a task (code)\n```\n\n1. [ ] ordered task",
      tags: [],
      updatedAt: 1_720_000_000_000,
      createdAt: 1_719_000_000_000,
      exportedPath: null,
      locked: false,
      shared: false,
    }),
  });
  await page.goto("/notes/n1");
  await page.getByRole("button", { name: "Preview", exact: true }).click();

  const preview = page.locator(".note-preview");
  const boxes = preview.getByRole("checkbox");
  await expect(boxes).toHaveCount(4);
  await expect(boxes.nth(0)).toHaveAttribute("aria-checked", "false");
  await expect(boxes.nth(1)).toHaveAttribute("aria-checked", "true");
  await expect(boxes.nth(0)).toHaveAttribute("tabindex", "0");
  await expect(preview.locator("li.task-list-item.is-done")).toHaveCount(1);
  // The raw marker text must not leak into the rendered item.
  await expect(preview.locator("li.task-list-item").first()).toHaveText("milk");
  // The fenced example stays literal code.
  await expect(preview.locator("pre code")).toContainText("- [ ] not a task (code)");
});

test("ticking a box in Preview flips that one marker and autosaves it", async ({ page }) => {
  await mockNotes(page, {
    get_note: (args: { id: string }) => ({
      id: args.id,
      title: "Todo",
      folderId: "nf1",
      markdown:
        "---\nstatus: open\n---\n# Groceries\n\n- [ ] milk\n- [x] eggs\n  - [ ] free-range\n\n```\n- [ ] not a task (code)\n```\n\n1. [ ] ordered task",
      tags: [],
      updatedAt: 1_720_000_000_000,
      createdAt: 1_719_000_000_000,
      exportedPath: null,
      locked: false,
      shared: false,
    }),
    save_note_text: (args: any) => {
      const w = window as any;
      w.__saved = [...(w.__saved ?? []), args.markdown];
      return 1_720_000_200_000;
    },
  });
  await page.goto("/notes/n1");
  await page.getByRole("button", { name: "Preview", exact: true }).click();

  const boxes = page.locator(".note-preview").getByRole("checkbox");
  await expect(boxes).toHaveCount(4);
  await boxes.nth(2).click(); // the nested "free-range"
  await expect(boxes.nth(2)).toHaveAttribute("aria-checked", "true");
  await boxes.nth(1).focus();
  await page.keyboard.press("Space"); // keyboard parity: un-tick "eggs"
  await expect(boxes.nth(1)).toHaveAttribute("aria-checked", "false");

  await expect
    .poll(() => page.evaluate(() => ((window as any).__saved ?? []).at(-1) ?? null), {
      timeout: 10_000,
    })
    .toBe(
      "---\nstatus: open\n---\n# Groceries\n\n- [ ] milk\n- [ ] eggs\n  - [x] free-range\n\n```\n- [ ] not a task (code)\n```\n\n1. [ ] ordered task",
    );

  // Back in Edit, the textarea carries the same source.
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await expect(page.locator(".body-area")).toHaveValue(/- \[x\] free-range/);
});

test("ticking a box in a meeting note preview persists through update_note", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      update_note: (args: any) => {
        const w = window as any;
        w.__updates = [...(w.__updates ?? []), args.markdown];
        return {
          meetingId: args.meetingId ?? args.id,
          providerId: "claude_code",
          markdown: args.markdown,
          exportedPath: null,
        };
      },
    },
    {
      audit_reminder_suggestions: [],
      get_meeting_detail: {
        locked: false,
        meeting: {
          id: "m-todo",
          startedAt: "2026-08-13T09:00:00Z",
          endedAt: "2026-08-13T10:00:00Z",
          title: "Planning",
          durationS: 3600,
          audioPath: null,
          status: "SUMMARIZED",
          folderId: null,
        },
        note: {
          meetingId: "m-todo",
          providerId: "claude_code",
          markdown: "# Planning\n\n## Action items\n- [ ] Send the deck\n- [ ] Book the room",
          exportedPath: null,
        },
        segments: [],
        assistantInteractions: [],
        aiProvider: "claude_code",
        aiModel: "gpt-5.6-codex",
        modelServed: "gpt-5.6-codex",
      },
    },
  );
  await page.goto("/meeting/m-todo");
  await page.getByRole("button", { name: "Preview", exact: true }).click();

  const boxes = page.locator("app-note-panel .note-preview").getByRole("checkbox");
  await expect(boxes).toHaveCount(2);
  await boxes.nth(1).click();
  await expect(boxes.nth(1)).toHaveAttribute("aria-checked", "true");
  await expect
    .poll(() => page.evaluate(() => ((window as any).__updates ?? []).at(-1) ?? null))
    .toBe("# Planning\n\n## Action items\n- [ ] Send the deck\n- [x] Book the room");
});

test("front matter TYPED into the body this session survives a Preview tick", async ({ page }) => {
  // Verifier finding: the note loaded WITHOUT front matter, the user types a `---`
  // block + checklist in Edit, then ticks in Preview. Re-splitting the emitted
  // markdown used to hand the typed block to the (empty) prefix and drop it on save.
  await mockNotes(page, {
    get_note: (args: { id: string }) => ({
      id: args.id,
      title: "Todo",
      folderId: "nf1",
      markdown: "",
      tags: [],
      updatedAt: 1_720_000_000_000,
      createdAt: 1_719_000_000_000,
      exportedPath: null,
      locked: false,
      shared: false,
    }),
    save_note_text: (args: any) => {
      const w = window as any;
      w.__saved = [...(w.__saved ?? []), args.markdown];
      return 1_720_000_200_000;
    },
  });
  await page.goto("/notes/n1");
  await page.locator(".body-area").fill("---\ntags: [shopping]\n---\n- [ ] buy milk");
  await page.getByRole("button", { name: "Preview", exact: true }).click();
  const box = page.locator(".note-preview").getByRole("checkbox");
  await expect(box).toHaveCount(1);
  await box.click();
  await expect(box).toHaveAttribute("aria-checked", "true");
  await expect
    .poll(() => page.evaluate(() => ((window as any).__saved ?? []).at(-1) ?? null), {
      timeout: 10_000,
    })
    .toBe("---\ntags: [shopping]\n---\n- [x] buy milk");
});

test("a failed meeting-note save drops the toggles queued behind it and rolls back", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      update_note: (args: any) => {
        const w = window as any;
        w.__updates = [...(w.__updates ?? []), args.markdown];
        // Held until the test fails it, so the second click is provably queued.
        return new Promise((_resolve, reject) => {
          w.__failSave = () => reject("disk full");
        });
      },
    },
    { audit_reminder_suggestions: [], get_meeting_detail: MEETING_TODO },
  );
  await page.goto("/meeting/m-todo");
  await page.getByRole("button", { name: "Preview", exact: true }).click();
  const boxes = page.locator("app-note-panel .note-preview").getByRole("checkbox");
  await expect(boxes).toHaveCount(2);
  await boxes.nth(0).click();
  await boxes.nth(1).click(); // queued behind the in-flight save
  await expect(boxes.nth(1)).toHaveAttribute("aria-checked", "true");
  await page.evaluate(() => (window as any).__failSave());
  // The view rolls back to what is on disk (both unchecked), preview still open ...
  await expect(boxes.nth(0)).toHaveAttribute("aria-checked", "false", { timeout: 10_000 });
  await expect(boxes.nth(1)).toHaveAttribute("aria-checked", "false");
  // ... and the queued second toggle was never written.
  await page.waitForTimeout(800);
  expect(await page.evaluate(() => (window as any).__updates)).toEqual([
    "# Planning\n\n## Action items\n- [x] Send the deck\n- [ ] Book the room",
  ]);
});

test.describe("toggleTask (source mapping)", () => {
  test("flips exactly the Nth task, ignoring code fences", () => {
    expect(toggleTask(NOTE_BODY, 0)).toBe(NOTE_BODY.replace("- [ ] milk", "- [x] milk"));
    expect(toggleTask(NOTE_BODY, 1)).toBe(NOTE_BODY.replace("- [x] eggs", "- [ ] eggs"));
    expect(toggleTask(NOTE_BODY, 3)).toBe(
      NOTE_BODY.replace("1. [ ] ordered task", "1. [x] ordered task"),
    );
  });

  test("handles uppercase X, * and + bullets, blockquotes and 1) lists", () => {
    const src = "* [X] a\n+ [ ] b\n> - [ ] quoted\n\n1) [ ] paren";
    expect(taskStates(src)).toEqual([true, false, false, false]);
    expect(toggleTask(src, 0)).toBe(src.replace("* [X] a", "* [ ] a"));
    expect(toggleTask(src, 2)).toBe(src.replace("> - [ ] quoted", "> - [x] quoted"));
    expect(toggleTask(src, 3)).toBe(src.replace("1) [ ] paren", "1) [x] paren"));
  });

  test("an indented code block that looks like a task is not counted", () => {
    const src = "Intro\n\n    - [ ] code, not a task\n\n- [ ] real";
    expect(taskStates(src)).toEqual([false]);
    expect(toggleTask(src, 0)).toBe(src.replace("- [ ] real", "- [x] real"));
  });

  test("verifies against the RENDERED view, not the raw source", () => {
    // Verifier repros: preprocessing (a multi-line <img>, a stripped front-matter
    // block) shifts which line is code, so the raw lexer "verified" the wrong line.
    const cases: Array<[string, string, string]> = [
      ['<img x="\n```\n">\n- [ ] b\n```\n- [ ] c', "- [ ] b", "- [x] b"],
      ["```yaml\nk: v ```\n- [ ] a\n```\n- [ ] b", "- [ ] a", "- [x] a"],
      ["---\n```\n---\n- [ ] b\n```\n- [ ] c", "- [ ] b", "- [x] b"],
    ];
    for (const [src, from, to] of cases) {
      expect(taskStates(preprocessMarkdown(src))).toEqual([false]);
      expect(toggleTask(src, 0, preprocessMarkdown)).toBe(src.replace(from, to));
    }
  });

  test("an empty marker is not a task, and out-of-range indexes are refused", () => {
    expect(taskStates("- [ ]\n- [ ] one")).toEqual([false]);
    expect(toggleTask("- [ ]\n- [ ] one", 0)).toBe("- [ ]\n- [x] one");
    expect(toggleTask("- [ ] one", 1)).toBeNull();
    expect(toggleTask("- [ ] one", -1)).toBeNull();
  });
});
