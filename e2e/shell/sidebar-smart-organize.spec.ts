import { expect, test, type Page } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

const availability = { selectable: true, here: false, self: false, descendant: false, locked: false, confirm: false, incompatible: false };
const SPACE = { id: "space-1", name: "Acme", kind: "meeting", level: "project", emoji: null, tint: null, locked: false, unlocked: false, isRoot: false, folders: [], groups: [] };
const CHROME_FOREST = Array.from({ length: 14 }, (_, index) => ({
  ...SPACE,
  id: `space-${index + 1}`,
  name: index === 0 ? "Acme" : `Workspace ${index + 1}`,
}));
const PLAN = {
  planId: "plan-1", totalScanned: 2, alreadyThere: 0, deferred: 0,
  newFolders: 1, reusedFolders: 0, timezoneLabel: "Europe/Warsaw (+02:00)",
  buckets: [{
    bucketId: "day:2026-09-19", folderName: "2026-09-19",
    destinationBreadcrumb: "Acme / 2026-09-19", status: "new",
    items: [
      { itemId: "meeting-1", kind: "meeting", title: "Platform standup", fromContainerId: "space-1", fromBreadcrumb: "Acme", reason: "Started 2026-09-19 09:00 (+02:00)" },
      { itemId: "note-1", kind: "note", title: "Launch brief", fromContainerId: "space-1", fromBreadcrumb: "Acme", reason: "Created 2026-09-19 10:00 (+02:00)" },
    ],
  }], skipped: [],
};

async function open(page: Page): Promise<void> {
  await mockTauri(page, {
    get_related_picker_bootstrap: () => (window as unknown as { __smartPicker: unknown }).__smartPicker,
    plan_smart_organize: (args: unknown) => {
      const target = window as unknown as { __smartPlanArgs?: unknown[]; __smartPlans: unknown[]; __deferSmartPlan?: boolean; __releaseSmartPlan?: () => void };
      (target.__smartPlanArgs ??= []).push(args);
      const plan = target.__smartPlans[Math.min(target.__smartPlanArgs.length - 1, target.__smartPlans.length - 1)];
      if (target.__deferSmartPlan) return new Promise((resolve) => { target.__releaseSmartPlan = () => resolve(plan); });
      return plan;
    },
    apply_smart_organize_plan: (args: unknown) => {
      const target = window as unknown as { __smartApplyArgs?: unknown[]; __smartWrites?: string[]; __smartApplyResult?: unknown; __deferSmartApply?: boolean; __releaseSmartApply?: () => void };
      (target.__smartApplyArgs ??= []).push(args);
      (target.__smartWrites ??= []).push("apply_smart_organize_plan");
      if ((window as unknown as { __rejectSmartApply?: boolean }).__rejectSmartApply) throw new Error("Connection interrupted");
      const result = target.__smartApplyResult ?? { applied: [{ itemId: "meeting-1", kind: "meeting", title: "Platform standup", fromContainerId: "space-1", toContainerId: "day-1", fromBreadcrumb: "Acme", toBreadcrumb: "Acme / 2026-09-19", bucketId: "day:2026-09-19", folderName: "2026-09-19", bucketStatus: "new" }], failures: [] };
      if (target.__deferSmartApply) return new Promise((resolve) => { target.__releaseSmartApply = () => resolve(result); });
      return result;
    },
    create_folder: () => { ((window as unknown as { __smartWrites?: string[] }).__smartWrites ??= []).push("create_folder"); return "unexpected"; },
    move_container: () => { ((window as unknown as { __smartWrites?: string[] }).__smartWrites ??= []).push("move_container"); return null; },
    move_note_doc: () => { ((window as unknown as { __smartWrites?: string[] }).__smartWrites ??= []).push("move_note_doc"); return null; },
    file_recording: () => { ((window as unknown as { __smartWrites?: string[] }).__smartWrites ??= []).push("file_recording"); return null; },
    discard_smart_organize_plan: () => null,
  }, { list_workspace_tree: [SPACE], list_container_items: { kind: "meeting", total: 0, items: [] } });
  await page.addInitScript(({ picker, plans }) => {
    const target = window as unknown as { __smartPicker: unknown; __smartPlans: unknown[] };
    target.__smartPicker = picker;
    target.__smartPlans = plans;
  }, {
    plans: [PLAN, { ...PLAN, planId: "plan-2" }],
    picker: {
      spaces: [{ ...SPACE, linkable: true, availability }], unclassified: [], anchor: null,
      destination: { sourceKind: "container", sourceLocked: false, currentContainerId: "space-1", currentPath: ["space-1"], root: { kind: "unfiled", label: "Not classified", containerId: null, availability }, container: null },
    },
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Smart organize notes and recordings" }).click();
  await page.getByRole("button", { name: /Choose a Workspace, folder/ }).click();
  const picker = page.getByRole("dialog", { name: "Choose a scope" });
  await picker.getByRole("button", { name: "Choose Acme" }).click();
  await picker.getByRole("button", { name: "Choose scope" }).click();
}

async function openWorkspaceChrome(page: Page): Promise<void> {
  await page.setViewportSize({ width: 1280, height: 480 });
  await mockTauri(page, { create_folder: () => ({ id: "f-new" }) }, {
    list_workspace_tree: CHROME_FOREST,
    list_container_items: { kind: "meeting", total: 0, items: [] },
  });
  await page.goto("/");
}

test("workspace menu stays on the shared primitive and closes after its action", async ({ page }) => {
  await openWorkspaceChrome(page);
  await page.getByRole("button", { name: "Actions for Acme" }).click();
  const item = page.getByRole("menuitem", { name: "Create folder here" });
  await expect(item).toBeVisible();
  await item.click();
  await expect(item).toHaveCount(0);
});

test("workspace scroller does not leave a partial first row under its fixed header", async ({ page }) => {
  await openWorkspaceChrome(page);
  const body = page.locator(".primary-sidebar .sb-scroll");
  await body.evaluate((element) => { element.scrollTop = 56; });
  await expect.poll(() => body.evaluate((element) => {
    const top = element.getBoundingClientRect().top;
    return Array.from(element.querySelectorAll<HTMLElement>(".tree > *")).filter((row) => {
      const rect = row.getBoundingClientRect();
      return rect.top < top && rect.bottom > top;
    }).length;
  })).toBe(0);
});

test("Smart organize previews before any write and Apply sends only approved ids", async ({ page }) => {
  await open(page);
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await expect(sheet).toContainText("recordings by the day they started and notes by the day they were created");
  expect(await page.evaluate(() => (window as unknown as { __smartWrites?: string[] }).__smartWrites ?? [])).toEqual([]);
  expect(await page.evaluate(() => (window as unknown as { __smartApplyArgs?: unknown[] }).__smartApplyArgs ?? [])).toEqual([]);
  expect(await page.evaluate(() => (window as unknown as { __smartWrites?: string[] }).__smartWrites ?? [])).toEqual([]);
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await expect(sheet.getByText("2026-09-19", { exact: true })).toBeVisible();
  await expect(sheet.getByText("New folder", { exact: true })).toBeVisible();
  await sheet.getByText("Launch brief", { exact: true }).locator("xpath=ancestor::label").getByRole("checkbox").uncheck();
  expect(await page.evaluate(() => (window as unknown as { __smartApplyArgs?: unknown[] }).__smartApplyArgs ?? [])).toEqual([]);
  expect(await page.evaluate(() => (window as unknown as { __smartWrites?: string[] }).__smartWrites ?? [])).toEqual([]);
  await sheet.getByRole("button", { name: "Apply 1 moves" }).click();
  await expect.poll(() => page.evaluate(() => (window as unknown as { __smartApplyArgs?: unknown[] }).__smartApplyArgs ?? [])).toEqual([{ request: { planId: "plan-1", selectedItemIds: ["meeting-1"] } }]);
});

test("excluding the whole bucket leaves Apply disabled and invokes no write", async ({ page }) => {
  await open(page);
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await sheet.getByRole("checkbox", { name: "Include 2026-09-19" }).uncheck();
  await expect(sheet.getByRole("button", { name: "Apply 0 moves" })).toBeDisabled();
  expect(await page.evaluate(() => (window as unknown as { __smartApplyArgs?: unknown[] }).__smartApplyArgs ?? [])).toEqual([]);
});

test("Preview again obtains a fresh plan id and never resubmits the consumed plan", async ({ page }) => {
  await open(page);
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await sheet.getByRole("button", { name: "Apply 2 moves" }).click();
  await sheet.getByRole("button", { name: "Preview again" }).click();
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await sheet.getByRole("button", { name: "Apply 2 moves" }).click();
  await expect.poll(() => page.evaluate(() => (window as unknown as { __smartApplyArgs?: unknown[] }).__smartApplyArgs ?? [])).toEqual([
    { request: { planId: "plan-1", selectedItemIds: ["meeting-1", "note-1"] } },
    { request: { planId: "plan-2", selectedItemIds: ["meeting-1", "note-1"] } },
  ]);
});

test("closing during a late preview synchronously scrubs and rejects the reply", async ({ page }) => {
  await open(page);
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await page.evaluate(() => { (window as unknown as { __deferSmartPlan?: boolean }).__deferSmartPlan = true; });
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await sheet.getByRole("button", { name: "Close Smart organize" }).click();
  await page.evaluate(() => { (window as unknown as { __releaseSmartPlan?: () => void }).__releaseSmartPlan?.(); });
  await expect(sheet).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __smartApplyArgs?: unknown[] }).__smartApplyArgs ?? [])).toEqual([]);
});

test("partial receipt renders only backend-reported facts and requires a fresh preview", async ({ page }) => {
  await open(page);
  await page.evaluate(() => {
    (window as unknown as { __smartApplyResult?: unknown }).__smartApplyResult = {
      applied: [{ itemId: "meeting-1", kind: "meeting", title: "Platform standup", fromContainerId: "space-1", toContainerId: "day-1", fromBreadcrumb: "Acme", toBreadcrumb: "Acme / 2026-09-19", bucketId: "day:2026-09-19", folderName: "2026-09-19", bucketStatus: "new" }],
      failures: [{ itemId: "note-1", title: "Launch brief", reason: "Source changed after preview", retryable: false }],
    };
  });
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await sheet.getByRole("button", { name: "Apply 2 moves" }).click();
  await expect(sheet.getByRole("heading", { name: "Organization finished" })).toBeVisible();
  await expect(sheet).toContainText("Acme → Acme / 2026-09-19");
  await expect(sheet).toContainText("Source changed after preview");
  await expect(sheet.getByRole("button", { name: "Preview again" })).toBeVisible();
  await expect(sheet).toContainText("Applied results are shown above.");
});

test("privacy invalidation during Apply closes the sheet and drops the late receipt", async ({ page }) => {
  await open(page);
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await page.evaluate(() => { (window as unknown as { __deferSmartApply?: boolean }).__deferSmartApply = true; });
  await sheet.getByRole("button", { name: "Apply 2 moves" }).click();
  await page.evaluate(() => {
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit("murmur://ask-history-invalidated", null);
  });
  await expect(sheet).toHaveCount(0);
  await page.evaluate(() => { (window as unknown as { __releaseSmartApply?: () => void }).__releaseSmartApply?.(); });
  await page.getByRole("button", { name: "Smart organize notes and recordings" }).click();
  await expect(page.getByRole("heading", { name: "Organization finished" })).toHaveCount(0);
});


test("an empty plan with skipped items does not claim everything is organized", async ({ page }) => {
  await open(page);
  await page.evaluate(() => {
    const target = window as unknown as { __smartPlans: unknown[] };
    target.__smartPlans = [{ planId: "empty-plan", totalScanned: 2, alreadyThere: 0, deferred: 0, newFolders: 0, reusedFolders: 0, timezoneLabel: "local calendar", buckets: [], skipped: [{ itemId: "meeting-1", title: "Unrelated recording", code: "noMatch", reason: "No shared direct relation" }] }];
  });
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await expect(sheet.getByText("No moves proposed", { exact: true })).toBeVisible();
  await expect(sheet.getByText("Everything is already organized", { exact: true })).toHaveCount(0);
});

test("an uncertain apply cannot claim nothing has moved", async ({ page }) => {
  await open(page);
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await page.evaluate(() => { (window as unknown as { __rejectSmartApply?: boolean }).__rejectSmartApply = true; });
  await sheet.getByRole("button", { name: "Apply 2 moves" }).click();
  await expect(sheet.getByRole("alert")).toContainText("Connection interrupted");
  await expect(sheet.locator("footer")).toContainText("Some items may have moved");
  await expect(sheet.getByText("Nothing has moved yet.", { exact: true })).toHaveCount(0);
});

test("the reviewed selection is frozen while Apply is in flight", async ({ page }) => {
  await open(page);
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await page.evaluate(() => { (window as unknown as { __deferSmartApply?: boolean }).__deferSmartApply = true; });
  await sheet.getByRole("button", { name: "Apply 2 moves" }).click();
  await expect(sheet.getByRole("checkbox", { name: "Include 2026-09-19" })).toBeDisabled();
  await expect(sheet.getByText("Launch brief", { exact: true }).locator("xpath=ancestor::label").getByRole("checkbox")).toBeDisabled();
});

test("a selected scope cannot restore private labels after synchronous invalidation", async ({ page }) => {
  await open(page);
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await sheet.getByRole("button", { name: "Place Acme" }).click();
  const picker = page.getByRole("dialog", { name: "Choose a scope" });
  await picker.getByRole("button", { name: "Choose Acme" }).click();
  await picker.getByRole("button", { name: "Choose scope", exact: true }).evaluate((button) => {
    (button as HTMLButtonElement).click();
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void }).__demoEmit("murmur://ask-history-invalidated", null);
  });
  await expect(sheet).toHaveCount(0);
  await page.getByRole("button", { name: "Smart organize notes and recordings" }).click();
  await expect(sheet.getByRole("button", { name: /Choose a Workspace, folder/ })).toBeVisible();
});


test("editing choices discards a preview without moving anything", async ({ page }) => {
  await open(page);
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  await sheet.getByRole("button", { name: "Edit choices" }).click();
  await expect(sheet.getByRole("button", { name: "Place Acme" })).toBeEnabled();
  await sheet.getByRole("radio", { name: /Group related recordings/ }).check();
  await expect(sheet).toContainText("Manual links, wikilinks, and accepted semantic links count");
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  expect(await page.evaluate(() => (window as unknown as { __smartPlanArgs?: unknown[] }).__smartPlanArgs?.at(-1))).toEqual({ request: { sourceContainerId: "space-1", destinationParentId: "space-1", includeDescendants: true, kinds: ["meeting"], rule: "byRelation" } });
  expect(await page.evaluate(() => (window as unknown as { __smartWrites?: string[] }).__smartWrites ?? [])).toEqual([]);
});

test("Not classified requires a real destination through the same picker", async ({ page }) => {
  await open(page);
  const sheet = page.getByRole("dialog", { name: "Smart organize" });
  await sheet.getByRole("button", { name: "Place Acme" }).click();
  const picker = page.getByRole("dialog", { name: "Choose a scope" });
  await picker.getByRole("button", { name: "Choose Not classified" }).click();
  await picker.getByRole("button", { name: "Choose scope", exact: true }).click();
  await expect(sheet.getByRole("button", { name: "Preview moves" })).toBeDisabled();
  await sheet.getByRole("button", { name: /New folders go in/ }).click();
  await expect(picker.getByRole("button", { name: "Choose Not classified" })).toHaveCount(0);
  await picker.getByRole("button", { name: "Choose Acme" }).click();
  await picker.getByRole("button", { name: "Choose scope", exact: true }).click();
  await sheet.getByRole("button", { name: "Preview moves" }).click();
  expect(await page.evaluate(() => (window as unknown as { __smartPlanArgs?: unknown[] }).__smartPlanArgs?.at(-1))).toEqual({ request: { sourceContainerId: null, destinationParentId: "space-1", includeDescendants: false, kinds: ["meeting"], rule: "byDay" } });
});
