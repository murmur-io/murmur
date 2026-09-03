import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * The same recovery, reachable without opening the broken meeting first.
 *
 * Somebody scanning the list for the recording that failed should not have to open it to find out
 * that it can be run again. The row's ⋯ menu carries the offer under the same two conditions the
 * backend refuses on: the status is still Error, and there is audio to work from.
 *
 * As in the meeting-view spec, the FIRST test is the control: the two absence assertions below
 * would pass against a build with no menu item at all.
 */

const ROW = `{
  id: "m-failed",
  startedAt: "2026-09-01T09:00:00Z",
  endedAt: "2026-09-01T09:42:00Z",
  title: "Planning sync",
  durationS: 2520,
  audioPath: "/Users/demo/audio/m-failed.wav",
  status: "ERROR",
  folderId: null,
}`;

test("a failed row offers the retry in its actions menu, and the list re-reads after it runs", async ({
  page,
}) => {
  await mockTauri(page, {
    list_meetings: new Function(
      "args",
      `globalThis.__listReads = (globalThis.__listReads || 0) + 1;
       return [${ROW}];`,
    ) as (args: unknown) => unknown,
    retry_transcription: new Function(
      "args",
      `globalThis.__retried = globalThis.__retried || [];
       globalThis.__retried.push(args.meetingId);
       return { meetingId: args.meetingId, notePath: null };`,
    ) as (args: unknown) => unknown,
  });

  await page.goto("/library");
  await expect(page.getByText("Planning sync")).toBeVisible({ timeout: 10_000 });

  await page.locator("li.row-item").first().hover();
  await page.getByRole("button", { name: "Meeting actions" }).first().click();
  const retry = page.getByRole("menuitem", { name: "Transcribe again" });
  await expect(retry).toBeVisible();

  const readsBefore = await page.evaluate(
    () => (globalThis as unknown as { __listReads?: number }).__listReads ?? 0,
  );
  await retry.click();

  // The id sent must be the meeting's own, not the `meeting:`-namespaced row key the list tracks by.
  await expect
    .poll(() =>
      page.evaluate(() => (globalThis as unknown as { __retried?: string[] }).__retried ?? []),
    )
    .toEqual(["m-failed"]);

  await expect
    .poll(() =>
      page.evaluate(() => (globalThis as unknown as { __listReads?: number }).__listReads ?? 0),
    )
    .toBeGreaterThan(readsBefore);
});

test("a row with no audio left offers nothing to run again", async ({ page }) => {
  await mockTauri(page, {
    list_meetings: new Function(
      "args",
      `const r = ${ROW};
       r.audioPath = null;
       return [r];`,
    ) as (args: unknown) => unknown,
  });

  await page.goto("/library");
  await expect(page.getByText("Planning sync")).toBeVisible({ timeout: 10_000 });
  await page.locator("li.row-item").first().hover();
  await page.getByRole("button", { name: "Meeting actions" }).first().click();
  await expect(page.getByRole("menuitem", { name: "Move to folder…" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Transcribe again" })).toHaveCount(0);
});

test("a healthy row offers nothing to run again", async ({ page }) => {
  await mockTauri(page, {
    list_meetings: new Function(
      "args",
      `const r = ${ROW};
       r.status = "SUMMARIZED";
       return [r];`,
    ) as (args: unknown) => unknown,
  });

  await page.goto("/library");
  await expect(page.getByText("Planning sync")).toBeVisible({ timeout: 10_000 });
  await page.locator("li.row-item").first().hover();
  await page.getByRole("button", { name: "Meeting actions" }).first().click();
  await expect(page.getByRole("menuitem", { name: "Move to folder…" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Transcribe again" })).toHaveCount(0);
});
