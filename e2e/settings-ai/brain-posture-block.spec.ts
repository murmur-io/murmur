import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Integration spec for BrainPostureBlockComponent (Task 2).
 *
 * The mock stalls `download_brain_model` forever so the progress state stays
 * visible long enough for the assertions. `brain_posture` starts as "cloud"
 * (heavy model absent) so we can observe the transition to pending state.
 */
test.describe("brain-posture-block", () => {
  test.beforeEach(async ({ page }) => {
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
      // Never resolves → brainDownloadingId stays set → progress bar stays visible.
      download_brain_model: () => new Promise(() => {}),
    });
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
  });

  test("(a) renders three posture cards — Cloud, Hybrid, Fully local", async ({
    page,
  }) => {
    // Use subtitle text for Cloud — its title text "Cloud" also appears inside the
    // Hybrid button's subtitle ("Cloud notes + on-device reactions"), so a bare /Cloud/
    // would hit two buttons and trigger a strict-mode violation.
    await expect(
      page.getByRole("button", { name: /Your Default AI does everything/ }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: /Hybrid/ }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: /Fully local/ }),
    ).toBeVisible();
  });

  test("(b) Enable Murmur Brain Live is absent", async ({ page }) => {
    await expect(page.getByText("Enable Murmur Brain Live")).toHaveCount(0);
  });

  test("(c) Fully local ASKS first (names the model), then confirming shows the download + Cancel", async ({
    page,
  }) => {
    await page.getByRole("button", { name: /Fully local/ }).click();
    // Confirm-first: the card appears and names the model + size — nothing downloads yet.
    await expect(page.getByText(/Fully local needs/)).toBeVisible();
    await expect(page.getByText(/Qwen3 4B/)).toBeVisible();
    // Explicit opt-in → the stalled download keeps brainDownloadingId set → the
    // download state (progress + a Cancel button) appears.
    await page
      .getByRole("button", { name: /Download .* enable Fully local/ })
      .click();
    await expect(
      page.getByRole("button", { name: "Cancel" }),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("(d) cloud right-now line renders postureStateLine copy", async ({
    page,
  }) => {
    // postureStateLine() for "cloud" = "Claude Code writes everything — ..."
    await expect(
      page.getByText(/Claude Code writes everything/),
    ).toBeVisible();
  });
});
