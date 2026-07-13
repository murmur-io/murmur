import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Regression test for the onboarding Model step's stale-response guard.
 *
 * `onLanguage`/`onModelSize` each call `refreshModelPresence()`, which does
 * `modelPresent.set(null)` → `await persistConfig()` → `modelPresent.set(await
 * ipc.modelPresent())`. Rapidly switching the Quality dropdown fires two
 * overlapping calls; without a stale-result guard, whichever `model_present`
 * IPC call resolves LAST wins, even if it belongs to an earlier, no-longer-
 * selected quality. This test forces the FIRST call's `model_present`
 * ("small", present) to resolve AFTER the second one ("medium", absent —
 * opposite of call order) and asserts the final `modelPresent()` state
 * matches the CURRENTLY selected quality ("medium", absent), not the stale
 * first pick.
 */
test("rapid Quality switches don't leave a stale modelPresent state", async ({
  page,
}) => {
  await mockTauri(page, {
    // "small" resolves SLOW (present); "medium" resolves FAST (absent) — so
    // if there is no stale-result guard, the slow "small" response lands
    // LAST and overwrites the fast "medium" response, even though "medium"
    // is the currently selected quality by the time both probes settle.
    model_present: (args: any) => {
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

  await page.goto("/onboarding");
  await page.getByRole("button", { name: "Get started" }).click();

  // Let the initial onEnterStep("model") probe (whatever the demo default
  // quality is) settle into SOME terminal state before starting the race.
  const qualitySelect = page.locator("select").nth(1);
  await expect(
    page.locator(".model-state .pill.is-success, .model-state button"),
  ).toBeVisible({ timeout: 5_000 });

  // Rapidly switch Quality small -> medium within the round-trip window: the
  // "small" probe (slow, 400ms) is still in flight when "medium" (fast,
  // 50ms) fires right after it.
  await qualitySelect.selectOption("small");
  await qualitySelect.selectOption("medium");

  // Wait past BOTH probes' resolution windows (400ms slow + 50ms fast) so the
  // late-resolving "small" response has had every chance to land and, on the
  // unguarded code, overwrite the correct "medium" result.
  await page.waitForTimeout(700);

  // The CURRENT selection is "medium", which is NOT present on disk — the
  // final state must reflect that (a "Download model" affordance), never the
  // stale "small" response landing later and claiming the model is ready.
  await expect(
    page.getByRole("button", { name: /Download model/ }),
  ).toBeVisible({ timeout: 5_000 });
  await expect(page.getByText("Model ready")).not.toBeVisible();
});
