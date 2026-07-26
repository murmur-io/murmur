import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Integration spec for the on-device models UI in its CURRENT home:
 * BrainEngineCardComponent (Advanced → Engines → "On this Mac — built-in
 * models") hosting ModelEffortPickerComponent behind its Configure disclosure.
 *
 * Superseded local-models-list.spec.ts — LocalModelsListComponent had no mount
 * site left once the models moved into the engine card + effort picker, and the
 * component was DELETED in the P0 foundations PR.
 */
test.describe("brain-engine-card (on-device models)", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page, {
      // Two models: light downloaded (card shows "Ready"), heavy not
      // downloaded (picker shows a Download button).
      list_brain_models: () => [
        {
          id: "bielik-1.5b",
          name: "Bielik 1.5B",
          class: "light",
          approxSizeBytes: 1_050_000_000,
          downloaded: true,
          selected: true,
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
    });
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
    // The engine card lives behind the ⚙ Advanced disclosure, in the
    // Engines → "On this Mac" group.
    await page.getByRole("button", { name: /Advanced/ }).click();
    await expect(page.locator("app-brain-engine-card")).toBeAttached();
  });

  // ── (a) — the built-in engine card is mounted under Engines ─────────────
  test("(a) app-brain-engine-card is mounted with the built-in models row", async ({
    page,
  }) => {
    await expect(
      page.getByText("On this Mac — built-in models"),
    ).toBeVisible();
  });

  // ── (b) — Ready pill when at least one registry GGUF is on disk ─────────
  test('(b) shows a "Ready" pill when a model is downloaded', async ({
    page,
  }) => {
    await expect(
      page.locator("app-brain-engine-card").getByText("Ready"),
    ).toBeVisible();
  });

  // ── (c) — Configure opens the effort picker with the mocked models ──────
  test("(c) Configure expands the model-effort picker listing the mocked models", async ({
    page,
  }) => {
    await page
      .locator("app-brain-engine-card")
      .getByRole("button", { name: /Configure/ })
      .click();
    await expect(page.locator("app-model-effort-picker")).toBeAttached();
    // The heavy model resolves as the notes/answers pick; the light model is
    // named in the automatic live-reactions note.
    await expect(page.getByText("Qwen3 4B")).toBeVisible();
    await expect(page.getByText("Bielik 1.5B")).toBeVisible();
  });

  // ── (d) — Download button on the not-downloaded heavy model ─────────────
  test("(d) shows a Download button for the not-downloaded model", async ({
    page,
  }) => {
    await page
      .locator("app-brain-engine-card")
      .getByRole("button", { name: /Configure/ })
      .click();
    await expect(
      page.getByRole("button", { name: /Download 2\.\d GB/ }),
    ).toBeVisible();
  });
});
