import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

test("a failed system-audio proof keeps the microphone live and explains why", async ({
  page,
}) => {
  await mockTauri(page, {
    model_present: () => true,
    recording_status: () => ({
      recording: true,
      meetingId: "m-live",
      startedAt: new Date(Date.now() - 30_000).toISOString(),
    }),
    is_mic_muted: () => false,
    set_mic_muted: () => {
      throw new Error("system audio has not produced a frame");
    },
  });

  await page.goto("/record");

  const mute = page.getByRole("button", { name: "Mute microphone" });
  await expect(mute).toBeVisible({ timeout: 10_000 });
  await mute.click();

  await expect(mute).toHaveAttribute("aria-pressed", "false");
  await expect(page.getByText(/Microphone stayed on/i)).toBeVisible();
  await expect(page.getByText(/Mic muted — still capturing others/i)).toHaveCount(0);
});

test("the mute control waits for its initial backend state before accepting input", async ({
  page,
}) => {
  await mockTauri(page, {
    model_present: () => true,
    recording_status: () => ({
      recording: true,
      meetingId: "m-live",
      startedAt: new Date(Date.now() - 30_000).toISOString(),
    }),
    is_mic_muted: () => {
      const target = window as unknown as {
        __resolveInitialMicState?: (muted: boolean) => void;
      };
      return new Promise<boolean>((resolve) => {
        target.__resolveInitialMicState = resolve;
      });
    },
    set_mic_muted: () => {
      const target = window as unknown as { __micMuteSetCalls?: number };
      target.__micMuteSetCalls = (target.__micMuteSetCalls ?? 0) + 1;
    },
  });

  await page.goto("/record");

  const mute = page.getByRole("button", { name: "Mute microphone" });
  await expect(mute).toBeVisible({ timeout: 10_000 });
  await expect(mute).toBeDisabled();
  await mute.click({ force: true });
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __micMuteSetCalls?: number })
            .__micMuteSetCalls ?? 0,
      ),
    )
    .toBe(0);

  await page.evaluate(() => {
    (
      window as unknown as {
        __resolveInitialMicState?: (muted: boolean) => void;
      }
    ).__resolveInitialMicState?.(false);
  });
  await expect(mute).toBeEnabled();
  await mute.click();
  await expect(mute).toHaveAttribute("aria-pressed", "true");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __micMuteSetCalls?: number })
            .__micMuteSetCalls ?? 0,
      ),
    )
    .toBe(1);
});

test("a backend watchdog auto-unmute resyncs the control and explains the recovery", async ({
  page,
}) => {
  await mockTauri(page, {
    model_present: () => true,
    recording_status: () => ({
      recording: true,
      meetingId: "m-live",
      startedAt: new Date(Date.now() - 30_000).toISOString(),
    }),
    is_mic_muted: () => false,
    set_mic_muted: () => undefined,
  });

  await page.goto("/record");

  const mute = page.getByRole("button", { name: "Mute microphone" });
  await expect(mute).toBeEnabled({ timeout: 10_000 });
  await mute.click();
  await expect(mute).toHaveAttribute("aria-pressed", "true");

  await page.evaluate(() => {
    (
      window as unknown as {
        __demoEmit: (event: string, payload: unknown) => void;
      }
    ).__demoEmit("murmur://mic-auto-unmuted", null);
  });

  await expect(mute).toHaveAttribute("aria-pressed", "false");
  await expect(page.getByText(/Microphone restored/i)).toBeVisible();
});

test("the initial mute snapshot waits until the auto-unmute listener is registered", async ({
  page,
}) => {
  const event = "murmur://mic-auto-unmuted";
  await mockTauri(
    page,
    {
      model_present: () => true,
      recording_status: () => ({
        recording: true,
        meetingId: "m-live",
        startedAt: new Date(Date.now() - 30_000).toISOString(),
      }),
      is_mic_muted: () => {
        (window as unknown as { __micSnapshotCalled?: boolean })
          .__micSnapshotCalled = true;
        return false;
      },
    },
    {},
    [],
    [event],
  );

  await page.goto("/record");

  const mute = page.getByRole("button", { name: "Mute microphone" });
  await expect(mute).toBeVisible({ timeout: 10_000 });
  await expect(mute).toBeDisabled();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __micSnapshotCalled?: boolean })
          .__micSnapshotCalled ?? false,
    ),
  ).toBe(false);

  await page.evaluate((eventName) => {
    (
      window as unknown as {
        __demoReleaseEventListeners: (name: string) => void;
      }
    ).__demoReleaseEventListeners(eventName);
  }, event);

  await expect(mute).toBeEnabled();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __micSnapshotCalled?: boolean })
            .__micSnapshotCalled ?? false,
      ),
    )
    .toBe(true);
});
