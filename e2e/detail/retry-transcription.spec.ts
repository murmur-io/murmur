import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * A recording whose transcription failed has to be recoverable from the app.
 *
 * The backend command existed all along, and both the ASR watchdog and the pipeline's terminal
 * guard tell the user in as many words to "use Retry transcription" — but nothing in the frontend
 * ever called it. So the one piece of advice on screen pointed at a control that did not exist, and
 * a meeting that failed mid-transcription was unrecoverable through the interface while its audio
 * sat intact on disk.
 *
 * The offer's two conditions are the backend's own refusals: the status is still Error, and there
 * is audio to work from.
 *
 * NOTE on the shape of these tests. The two "no offer" cases below would pass against a build where
 * the banner does not exist at all — absence is easy to satisfy. The FIRST test is what makes them
 * mean anything, so if it ever goes red, treat the other two as uninformative rather than green.
 * Overrides are serialized and run page-side, so each fixture is inlined rather than closed over.
 */

const ERRORED_DETAIL = `{
  meeting: {
    id: "m-failed",
    startedAt: "2026-09-01T09:00:00Z",
    endedAt: "2026-09-01T09:42:00Z",
    title: "Planning sync",
    durationS: 2520,
    audioPath: "/Users/demo/audio/m-failed.wav",
    status: "ERROR",
    folderId: null,
  },
  note: null,
  segments: [],
  assistantInteractions: [],
  aiProvider: null,
  aiModelRequested: null,
  aiModelServed: null,
}`;

test("a failed recording with audio on disk offers a retry, and the view re-reads after it runs", async ({
  page,
}) => {
  await mockTauri(page, {
    get_meeting_detail: new Function(
      "args",
      `globalThis.__detailReads = (globalThis.__detailReads || 0) + 1;
       return ${ERRORED_DETAIL};`,
    ) as (args: unknown) => unknown,
    retry_transcription: new Function(
      "args",
      `globalThis.__retried = globalThis.__retried || [];
       globalThis.__retried.push(args.meetingId);
       return { meetingId: args.meetingId, notePath: null };`,
    ) as (args: unknown) => unknown,
  });

  await page.goto("/meeting/m-failed");

  const retry = page.getByRole("button", { name: "Transcribe again" });
  await expect(retry).toBeVisible();

  const readsBefore = await page.evaluate(
    () => (globalThis as unknown as { __detailReads?: number }).__detailReads ?? 0,
  );
  await retry.click();

  await expect
    .poll(() =>
      page.evaluate(() => (globalThis as unknown as { __retried?: string[] }).__retried ?? []),
    )
    .toEqual(["m-failed"]);

  // Calling the command is only half of it. Without the re-read the user is left staring at the
  // failed state after a retry that worked.
  await expect
    .poll(() =>
      page.evaluate(
        () => (globalThis as unknown as { __detailReads?: number }).__detailReads ?? 0,
      ),
    )
    .toBeGreaterThan(readsBefore);
});

test("a failed recording with no audio left offers nothing — there is nothing to run again", async ({
  page,
}) => {
  await mockTauri(page, {
    get_meeting_detail: new Function(
      "args",
      `const d = ${ERRORED_DETAIL};
       d.meeting.id = "m-no-audio";
       d.meeting.audioPath = null;
       return d;`,
    ) as (args: unknown) => unknown,
  });

  await page.goto("/meeting/m-no-audio");
  await expect(page.getByRole("heading", { name: "Planning sync" }).first()).toBeVisible();
  await expect(page.getByRole("button", { name: "Transcribe again" })).toHaveCount(0);
});

test("a healthy meeting shows no retry offer", async ({ page }) => {
  await mockTauri(page, {
    get_meeting_detail: new Function(
      "args",
      `const d = ${ERRORED_DETAIL};
       d.meeting.id = "m-ok";
       d.meeting.status = "SUMMARIZED";
       return d;`,
    ) as (args: unknown) => unknown,
  });

  await page.goto("/meeting/m-ok");
  await expect(page.getByRole("heading", { name: "Planning sync" }).first()).toBeVisible();
  await expect(page.getByRole("button", { name: "Transcribe again" })).toHaveCount(0);
});
