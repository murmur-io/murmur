import { test, expect, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * LIVE-CAPTION readiness must be VISIBLE in the recorder, not just in a backend log line.
 *
 * The defect: on a fresh ≥ 12 GB Mac onboarding downloads only the heavy turbo batch default, and
 * the live tick deliberately never runs a medium/large encoder every 3 s (heat) — so live captions
 * never appeared, with nothing but a `warn!` to show for it. The backend now reports the state as
 * `AppConfigDto.liveCaptions` (`get_config`), and this spec pins the render contract:
 *
 *   (a) `modelMissing` + a present transcription model + not recording → the notice renders, with
 *       the retryable "download it" framing.
 *   (b) `pinnedHeavy` → the notice renders with the CONFIGURATION framing (a heavy live-model pin is
 *       a choice, not a failed download) and offers NO download action.
 *   (c) `ready` → nothing renders.
 *   (d) the key ABSENT entirely (an older backend / an unprobed config) → nothing renders.
 *   (e) while RECORDING → the footer's caption ticker says captions are off instead of showing an
 *       eternal, lying "Listening…".
 *
 * RED before GREEN: pre-fix there was NO recorder surface for this state at all — `.cc-notice` did
 * not exist and the footer showed "Listening…" for the whole meeting while no caption could ever
 * arrive — so (a), (b) and (e) fail against the unpatched FE.
 *
 * `__demoConfig` merges over the base mock's DEFAULT_CONFIG (which has no `liveCaptions` key), so
 * each case pins only its own delta.
 */

/**
 * Pin `get_config`'s live-caption state (or nothing at all, for the key-absent case).
 *
 * `overrides` lets a case swap a command — e.g. a `download_model` that never resolves, so the
 * in-flight companion fetch can be observed. The base mock re-reads `window.__demoConfig` on EVERY
 * `get_config`, so a case can also mutate it mid-test to simulate a model landing.
 */
async function bootRecord(
  page: Page,
  liveCaptions: string | null,
  overrides: Record<string, (args: unknown) => unknown> = {},
): Promise<void> {
  await mockTauri(page, { model_present: () => true, ...overrides });
  if (liveCaptions !== null) {
    await page.addInitScript((state: string) => {
      (
        window as unknown as { __demoConfig: Record<string, unknown> }
      ).__demoConfig = { liveCaptions: state };
    }, liveCaptions);
  }
  await page.goto("/record");
  // The idle start control proves the record screen mounted…
  await expect(page.locator("app-record button.start-btn")).toBeVisible({
    timeout: 10_000,
  });
  // …and the idle stats strip proves `ngOnInit` ran to COMPLETION — it renders only after the
  // `getConfig()` → `modelPresent()` → `getAnalytics()` chain resolved. Without this anchor the
  // "renders nothing" cases would pass trivially against a still-loading screen.
  await expect(page.locator("app-record .stats")).toBeVisible({
    timeout: 10_000,
  });
}

const notice = "app-record .cc-notice";

test.describe("Record — the no-live-captions notice", () => {
  test("(a) modelMissing renders the notice with a retryable download action", async ({
    page,
  }) => {
    await bootRecord(page, "modelMissing");

    await expect(page.locator(notice)).toBeVisible();
    await expect(page.locator(notice)).toContainText("Live captions are off");
    // The retryable framing — a download that didn't land, not a config choice.
    await expect(page.locator(notice)).toContainText(
      /small live-caption model isn't on this Mac/,
    );
    await expect(
      page.locator(notice).getByRole("button", { name: "Download it" }),
    ).toBeVisible();
    // Honest about what is NOT affected.
    await expect(page.locator(notice)).toContainText(/transcript still runs/i);
  });

  test("(b) pinnedHeavy renders the configuration framing and no download action", async ({
    page,
  }) => {
    await bootRecord(page, "pinnedHeavy");

    await expect(page.locator(notice)).toBeVisible();
    await expect(page.locator(notice)).toContainText(
      /live-caption model is a large one/,
    );
    // Nothing to fetch on the user's behalf — a heavy model is never run live.
    await expect(
      page.locator(notice).getByRole("button", { name: "Download it" }),
    ).toHaveCount(0);
    await expect(page.locator(notice)).not.toContainText(
      /download may have failed/,
    );
  });

  test("(c) ready renders no notice", async ({ page }) => {
    await bootRecord(page, "ready");
    await expect(page.locator(notice)).toHaveCount(0);
  });

  test("(d) an absent liveCaptions key renders no notice", async ({ page }) => {
    await bootRecord(page, null);
    await expect(page.locator(notice)).toHaveCount(0);
  });

  test("(e) while recording, the footer says captions are off instead of Listening…", async ({
    page,
  }) => {
    await bootRecord(page, "modelMissing");

    await page.locator("app-record button.start-btn").click();
    await expect(page.locator("app-record .rec-topbar")).toBeVisible({
      timeout: 10_000,
    });

    // The recording footer's ticker is replaced by the honest indicator…
    await expect(page.locator("app-record .rec-foot .cc-off")).toBeVisible();
    await expect(page.locator("app-record .rec-foot .cc-off")).toContainText(
      "Captions off",
    );
    // …no eternal "Listening…" placeholder, and the live-captions scope hint is gone too.
    await expect(page.locator("app-record .rec-foot .cc-idle")).toHaveCount(0);
    await expect(page.locator("app-record .rec-foot .cc-scope")).toHaveCount(0);
    // The banner-style notice stands down while recording (the footer carries the state).
    await expect(page.locator(notice)).toHaveCount(0);
  });

  test("(f) ready keeps the normal live-caption ticker while recording", async ({
    page,
  }) => {
    await bootRecord(page, "ready");

    await page.locator("app-record button.start-btn").click();
    await expect(page.locator("app-record .rec-topbar")).toBeVisible({
      timeout: 10_000,
    });

    await expect(page.locator("app-record .rec-foot .cc-line")).toBeVisible();
    await expect(page.locator("app-record .rec-foot .cc-off")).toHaveCount(0);
    await expect(page.locator("app-record .rec-foot .cc-scope")).toBeVisible();
  });

  test("(g) the companion retry never blocks Start", async ({ page }) => {
    // A `download_model` that never resolves keeps the companion fetch in flight for the whole
    // assertion window. RED before the fix: the notice's button drove the SAME `downloadingModel`
    // signal the model-absent banner does, and `canRecord` gates on it — so retrying the
    // live-caption companion disabled Start while the transcription model was already present,
    // contradicting the notice's own "recording is unaffected" copy.
    await bootRecord(page, "modelMissing", {
      download_model: () => new Promise(() => {}),
    });

    const start = page.locator("app-record button.start-btn");
    await expect(start).toBeEnabled();

    await page.locator(notice).getByRole("button", { name: "Download it" }).click();

    // In flight…
    await expect(
      page.locator(notice).getByRole("button", { name: "Downloading…" }),
    ).toBeDisabled();
    // …and recording stays available throughout.
    await expect(start).toBeEnabled();
    // The idle hint must not claim a download is blocking anything either.
    await expect(page.locator("app-record .rec-strip-hint")).not.toContainText(
      /you can start recording when it finishes/,
    );
  });

  test("(h) the notice clears when a model download finishes elsewhere", async ({
    page,
  }) => {
    // Live-caption readiness is a DEVICE/DISK fact: downloading a live-safe model from Settings
    // must clear this notice without a remount, or the UI keeps saying captions are off while
    // `start_recording` would happily run them. RED before the fix: `config()` was written once in
    // `ngOnInit` and never refreshed by anything but this screen's own action.
    await bootRecord(page, "modelMissing");
    await expect(page.locator(notice)).toBeVisible();

    await page.evaluate(() => {
      const w = window as unknown as {
        __demoConfig: Record<string, unknown>;
        __demoEmit: (event: string, payload: unknown) => void;
      };
      // The model landed — the very next `get_config` reports ready.
      w.__demoConfig["liveCaptions"] = "ready";
      w.__demoEmit("murmur://model-download", {
        downloaded: 1,
        total: 1,
        done: true,
      });
    });

    await expect(page.locator(notice)).toHaveCount(0);
  });

  test("(i) a retry that doesn't fix it says so instead of looking successful", async ({
    page,
  }) => {
    // `download_model` RESOLVES, but the companion fetch inside it failed — the backend swallows
    // that by design (the batch model is what gates recording), so the command succeeding proves
    // nothing. The refreshed readiness state is the real verdict.
    await bootRecord(page, "modelMissing", { download_model: () => "ok" });

    await page.locator(notice).getByRole("button", { name: "Download it" }).click();

    await expect(page.locator(`${notice} .cc-notice-error`)).toContainText(
      /still isn't on this Mac/,
    );
    // Still honest about the state itself — the notice does not quietly disappear.
    await expect(page.locator(notice)).toBeVisible();
  });
});

/**
 * The ONBOARDING half of the same fix. Lives beside the recorder spec because it pins the other
 * consumer of one backend decision (`live_captions::companion_size_for`, surfaced as
 * `AppConfigDto.liveCompanionPending`): the wizard must disclose the extra live-caption download
 * when — and only when — the backend would actually make it.
 *
 * RED before GREEN: the first version of this fix re-derived the rule in TypeScript from the size
 * NAME alone (`includes("large") || includes("medium")`), so it promised a companion transfer for
 * ANY heavy quality — including the cases the backend skips (the live pin disabled or itself heavy,
 * or a live-safe model already on disk). Case (a) fails against that copy.
 */
test.describe("Onboarding — the live-caption companion disclosure", () => {
  // Its OWN class — `.model-note` is shared with the size/one-time hints beside the button.
  const note = "app-onboarding .live-companion-note";

  /** Boot the wizard's Model step with a pinned config snapshot. */
  async function bootModelStep(
    page: Page,
    config: Record<string, unknown>,
  ): Promise<void> {
    await mockTauri(page, { model_present: () => false });
    await page.addInitScript((cfg: Record<string, unknown>) => {
      (
        window as unknown as { __demoConfig: Record<string, unknown> }
      ).__demoConfig = cfg;
    }, config);
    await page.goto("/onboarding");
    await page.getByRole("button", { name: "Get started" }).click();
    await expect(
      page.getByRole("button", { name: /Download model/ }),
    ).toBeVisible({ timeout: 10_000 });
  }

  test("(a) a heavy quality alone does NOT promise a second download", async ({
    page,
  }) => {
    // Heavy batch quality, but the backend says no companion is pending (e.g. a live-safe model is
    // already on disk, or the live pin is disabled) — the wizard must not invent a transfer.
    await bootModelStep(page, {
      modelSize: "large-v3",
      liveCompanionPending: false,
    });
    await expect(page.locator(note)).toHaveCount(0);
  });

  test("(b) the backend's pending companion IS disclosed", async ({ page }) => {
    await bootModelStep(page, {
      modelSize: "large-v3",
      liveCompanionPending: true,
    });
    await expect(page.locator(note)).toContainText(
      /Live captions also need a small model/,
    );
  });

  test("(c) an absent liveCompanionPending key discloses nothing", async ({
    page,
  }) => {
    await bootModelStep(page, { modelSize: "large-v3" });
    await expect(page.locator(note)).toHaveCount(0);
  });
});
