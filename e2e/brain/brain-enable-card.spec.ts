import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * RED-before-GREEN (live-found bug, v0.9.9): "Enable the brain" downloaded the heavy
 * GGUF successfully but never SELECTED it — `brain_model_present` resolves via the
 * persisted `brain_model_id`, which `download_brain_model` alone never writes. A fresh
 * install (no prior selection) would download the file, then still report it "missing"
 * forever, showing a misleading "download failed" banner — until something UNRELATED
 * (e.g. a brain-posture preset) happened to persist `brain_model_id` as a side effect.
 *
 * This mock proves the fix: `brain_model_present` here is backed by whether
 * `select_brain_model` was ever called (mirrors the real backend's `brain_model_id`
 * read) — `download_brain_model` alone does NOT flip it. Pre-fix, `enable()` never
 * calls `select_brain_model`, so this test fails (banner stays up forever). Post-fix,
 * `enable()` explicitly selects the model it just downloaded.
 */
test("Enable the brain: a successful download is also SELECTED, not just fetched", async ({
  page,
}) => {
  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });
  page.on("pageerror", (err) => consoleErrors.push(String(err)));

  await mockTauri(page, {
    list_brain_models: () => [
      {
        id: "qwen3-4b-instruct-2507",
        name: "Qwen3 4B Instruct 2507 (heavy · multilingual)",
        filename: "Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        url: "https://huggingface.co/example/model.gguf",
        approxSizeBytes: 2_500_000_000,
        minRamGb: 6,
        languages: ["en", "multi", "pl"],
        arch: "qwen3",
        class: "heavy",
        downloaded: false,
        fitsRam: true,
        selected: false,
        selectedLight: false,
        selectedHeavy: true,
      },
    ],
    // The download command "succeeds" (mirrors the file already existing / a clean
    // fetch) but — exactly like the real backend — does NOT itself persist a selection.
    download_brain_model: () => null,
    // Presence is gated on selection having happened, mirroring `brain_model_present`
    // reading the persisted `brain_model_id` that ONLY `select_brain_model` writes.
    brain_model_present: () => !!(window as unknown as { __brainSelected?: boolean }).__brainSelected,
    select_brain_model: () => {
      (window as unknown as { __brainSelected?: boolean }).__brainSelected = true;
      return null;
    },
    embed_model_present: () => true, // isolate the test to the brain leg only
  });

  await page.goto("/brain");
  const card = page.locator("app-brain-enable-card");
  await expect(card).toBeVisible();

  await card.getByRole("button", { name: "Enable the brain" }).click();

  // The success path: no "download failed" banner, the card disappears (brainReady()).
  await expect(page.getByText(/download|can't find/i)).toHaveCount(0, { timeout: 5000 });
  await expect(card).toHaveCount(0);

  expect(consoleErrors).toEqual([]);
});
