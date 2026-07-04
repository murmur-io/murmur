import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Verifies that picking "Fully local" posture auto-downloads the absent heavy
 * model BEFORE committing the posture — the order is:
 *   1. download_brain_model (heavy, absent)
 *   2. select_brain_model  (newly downloaded heavy)
 *   3. set_brain_posture   (commit)
 */
test("picking Fully local auto-downloads the absent heavy model then commits", async ({
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
        languages: ["en", "pl"],
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
  // Click the "Fully local" posture button
  await page.getByRole("button", { name: /Fully local/ }).click();

  // The store must: (1) download the absent heavy model, (2) select it, (3) commit posture.
  await expect
    .poll(
      () =>
        page.evaluate(
          () => (window as any).__test_calls__ ?? [],
        ),
      { timeout: 15_000 },
    )
    .toEqual(["download:qwen3-4b", "select:qwen3-4b", "posture:fully_local"]);
});
