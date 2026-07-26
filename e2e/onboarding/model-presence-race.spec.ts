import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { mockTauri } from "../settings-ai/mock-invoke";

const RECOMMENDATION = JSON.parse(
  readFileSync(
    join(__dirname, "..", "fixtures", "whisper-recommendation.json"),
    "utf8",
  ),
);

/**
 * Regression test for the onboarding Model step's stale-response guard, rewritten
 * for the power slider (the Quality `<select>` this spec used to drive is gone).
 *
 * Picking a rung calls `refreshModelPresence()`, which does `modelPresent.set(null)`
 * → `await persistConfig()` → `modelPresent.set(await ipc.modelPresent())`. Two
 * rapid picks overlap; without `modelPresenceRequestId`, whichever `model_present`
 * IPC resolves LAST wins — even when it belongs to an earlier, no-longer-selected
 * size. Here the FIRST pick ("small", present) is forced to resolve AFTER the
 * second ("medium", absent), i.e. the opposite of call order, and the final state
 * must match the CURRENT selection.
 *
 * PROVING IT IS REALLY RED: temporarily delete the `modelPresenceRequestId`
 * comparison in `onboarding.component.ts::refreshModelPresence` and this test must
 * FAIL with "Model ready" visible. A rewrite that passes against unguarded code
 * captures nothing.
 */
test("rapid quality picks don't leave a stale modelPresent state", async ({
  page,
}) => {
  await mockTauri(page, {
    // The picker reads its ladder from here. Without this mock the demo mock's
    // `default:` branch returns null and no slider renders at all.
    whisper_recommendation: () => {
      const w = window as any;
      const cfg = (w.__demoConfig && w.__demoConfig.modelSize) || "";
      return Object.assign({}, (window as any).__recFixture, {
        selectedId: cfg || (window as any).__recFixture.selectedId,
      });
    },
    // "small" resolves SLOW (present); everything else resolves FAST (absent) — so
    // an unguarded implementation lets the slow "small" answer land LAST and
    // overwrite the fast "medium" answer that belongs to the current selection.
    model_present: () => {
      const w = window as any;
      const size = (w.__demoConfig && w.__demoConfig.modelSize) || "";
      const delayMs = size === "small" ? 400 : 50;
      return new Promise((resolve) =>
        setTimeout(() => resolve(size === "small"), delayMs),
      );
    },
    save_config: (args: any) => {
      const w = window as any;
      w.__demoConfig = Object.assign({}, w.__demoConfig, args.config);
      return null;
    },
  });
  await page.addInitScript((rec) => {
    (window as any).__recFixture = rec;
  }, RECOMMENDATION);

  await page.goto("/onboarding");
  await page.getByRole("button", { name: "Get started" }).click();

  const slider = page.getByRole("slider", {
    name: "Transcription model power",
  });
  await expect(slider).toBeVisible({ timeout: 5_000 });

  // Let the initial probe settle into SOME terminal state before racing.
  await expect(
    page.locator(".model-state .pill.is-success, .model-state button"),
  ).toBeVisible({ timeout: 5_000 });

  // Balanced = `small` (slow, present), then Maximum = `large-v3` (fast, absent).
  // The rung ORDER is Light/Balanced/Sharp/Maximum, so indices 1 then 3.
  await slider.fill("1");
  await slider.dispatchEvent("change");
  await slider.fill("3");
  await slider.dispatchEvent("change");

  // Past BOTH windows (400 ms slow + 50 ms fast) so the late "small" answer has
  // every chance to land and, on unguarded code, overwrite the correct one.
  await page.waitForTimeout(700);

  // The CURRENT selection is not on disk, so the step must offer a download —
  // never the stale "small" answer arriving late and claiming the model is ready.
  await expect(page.getByRole("button", { name: /Download/ })).toBeVisible({
    timeout: 5_000,
  });
  await expect(page.getByText("Model ready")).not.toBeVisible();
});
