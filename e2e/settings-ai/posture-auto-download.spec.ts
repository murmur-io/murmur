import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Verifies the CONFIRM-FIRST flow: picking "Fully local" when a model is absent
 * shows a confirm card and starts NOTHING; only clicking "Download & enable" runs
 * the download, then selects ALL needed models (not just the newly-downloaded
 * subset) and commits. The expected order after confirming is:
 *   1. download_brain_model (heavy, absent)
 *   2. select_brain_model  (heavy — just downloaded)
 *   3. select_brain_model  (light — already on disk but not selected)
 *   4. set_brain_posture   (commit)
 */
test("picking Fully local asks first, then on confirm downloads + selects all needed + commits", async ({
  page,
}) => {
  await mockTauri(page, {
    list_brain_models: () => [
      {
        id: "bielik-1.5b",
        name: "Bielik 1.5B",
        class: "light",
        approxSizeBytes: 1_050_000_000,
        downloaded: true,
        selected: false,
        fitsRam: true,
        filename: "bielik-1.5b.gguf",
        url: "https://example.com/bielik-1.5b.gguf",
        minRamGb: 4,
        languages: ["pl", "en"],
        arch: "llama",
      },
      {
        id: "qwen3-4b",
        name: "Qwen3 4B",
        class: "heavy",
        approxSizeBytes: 2_300_000_000,
        downloaded: false,
        selected: false,
        fitsRam: true,
        filename: "qwen3-4b.gguf",
        url: "https://example.com/qwen3-4b.gguf",
        minRamGb: 8,
        // Registry-faithful: every Qwen row in `reason::BRAIN_MODELS` declares
        // "multi" — that tag is what puts a model in the Multilingual family.
        languages: ["en", "multi", "pl"],
        arch: "qwen3",
      },
    ],
    brain_posture: () => "cloud",
    brain_live_ram_ok: () => true,
    // Each override is self-contained (no closure over test-scope): writes
    // window.__test_calls__ which we read back from the test side.
    download_brain_model: (a: any) => {
      const w = window as any;
      w.__test_calls__ = w.__test_calls__ || [];
      w.__test_calls__.push("download:" + a.modelId);
      return null;
    },
    select_brain_model: (a: any) => {
      const w = window as any;
      w.__test_calls__ = w.__test_calls__ || [];
      w.__test_calls__.push("select:" + a.modelId);
      return null;
    },
    set_brain_posture: (a: any) => {
      const w = window as any;
      w.__test_calls__ = w.__test_calls__ || [];
      w.__test_calls__.push("posture:" + a.posture);
      return null;
    },
  });

  await page.goto("/settings");
  // Reveal the AI & Models section
  await page.getByText("AI & Models").first().click();
  // Click the "Fully local" posture button — this must ASK, not download.
  await page.getByRole("button", { name: /Fully local/ }).click();

  // Confirm card is shown; NOTHING has downloaded/committed on the single tap.
  await expect(page.getByText(/Fully local needs/)).toBeVisible();
  expect(await page.evaluate(() => (window as any).__test_calls__ ?? [])).toEqual(
    [],
  );

  // Explicit opt-in: click "Download & enable Fully local".
  await page
    .getByRole("button", { name: /Download .* enable Fully local/ })
    .click();

  // Now: (1) download the absent heavy model, (2) select ALL needed models in
  // neededModelsFor order (heavy → light), (3) commit posture.
  await expect
    .poll(
      () =>
        page.evaluate(
          () => (window as any).__test_calls__ ?? [],
        ),
      { timeout: 15_000 },
    )
    .toEqual([
      "download:qwen3-4b",
      "select:qwen3-4b",
      "select:bielik-1.5b",
      "posture:fully_local",
    ]);
});

/**
 * Verifies the FAST PATH: when all needed models are already downloaded,
 * the store selects them all (pinning roles to the auto-picked models) and
 * then commits — with NO download calls. This prevents the "already-downloaded
 * but not selected" model from being silently bypassed and the backend falling
 * back to its registry default (which may be a different, absent GGUF).
 */
test("picking Fully local with all models already downloaded selects all needed then commits (no download)", async ({
  page,
}) => {
  await mockTauri(page, {
    list_brain_models: () => [
      {
        id: "bielik-1.5b",
        name: "Bielik 1.5B",
        class: "light",
        approxSizeBytes: 1_050_000_000,
        downloaded: true,
        selected: false,
        fitsRam: true,
        filename: "bielik-1.5b.gguf",
        url: "https://example.com/bielik-1.5b.gguf",
        minRamGb: 4,
        languages: ["pl", "en"],
        arch: "llama",
      },
      {
        id: "qwen3-4b",
        name: "Qwen3 4B",
        class: "heavy",
        approxSizeBytes: 2_300_000_000,
        downloaded: true,
        selected: false,
        fitsRam: true,
        filename: "qwen3-4b.gguf",
        url: "https://example.com/qwen3-4b.gguf",
        minRamGb: 8,
        // Registry-faithful: every Qwen row in `reason::BRAIN_MODELS` declares
        // "multi" — that tag is what puts a model in the Multilingual family.
        languages: ["en", "multi", "pl"],
        arch: "qwen3",
      },
    ],
    brain_posture: () => "cloud",
    brain_live_ram_ok: () => true,
    download_brain_model: (a: any) => {
      const w = window as any;
      w.__test_calls__ = w.__test_calls__ || [];
      w.__test_calls__.push("download:" + a.modelId);
      return null;
    },
    select_brain_model: (a: any) => {
      const w = window as any;
      w.__test_calls__ = w.__test_calls__ || [];
      w.__test_calls__.push("select:" + a.modelId);
      return null;
    },
    set_brain_posture: (a: any) => {
      const w = window as any;
      w.__test_calls__ = w.__test_calls__ || [];
      w.__test_calls__.push("posture:" + a.posture);
      return null;
    },
  });

  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();
  await page.getByRole("button", { name: /Fully local/ }).click();

  // No download calls — both models already present. Selects heavy → light, then commits.
  await expect
    .poll(
      () =>
        page.evaluate(
          () => (window as any).__test_calls__ ?? [],
        ),
      { timeout: 15_000 },
    )
    .toEqual([
      "select:qwen3-4b",
      "select:bielik-1.5b",
      "posture:fully_local",
    ]);
});
