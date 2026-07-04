import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Integration spec for LocalModelsListComponent (Task 3).
 *
 * Reveal condition: `anyLocal()` in AiRoleRowsComponent is true when
 * `roleAskConnValue() === "local"` — injected via `window.__demoConfig` which
 * the base mock's `get_config` handler merges into its DEFAULT_CONFIG.  That
 * also triggers the _autoExpand effect so the role rows open without a manual
 * "Customize per feature" click.
 *
 * RED contract: the `app-local-models-list` custom element is NOT in the DOM
 * until the component is created AND wired into `ai-role-rows` — that
 * assertion drives the RED→GREEN arc.
 */
test.describe("local-models-list", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page, {
      // Two models: first downloaded+selected (shows "In use"), second not
      // downloaded (shows "Download" button).
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
          languages: ["en", "pl"],
          arch: "qwen3",
        },
      ],
    });
    // Set roleAskConnection=local so anyLocal() → true and _autoExpand opens
    // the role rows, making the models list visible without a manual click.
    await page.addInitScript(() => {
      (window as unknown as { __demoConfig: Record<string, unknown> }).__demoConfig =
        { roleAskConnection: "local" };
    });
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
    // Wait for the models block to render (load() is async; effect fires after config loads).
    await expect(page.locator(".brain-models")).toBeVisible({ timeout: 10_000 });
  });

  // ── (a) — RED before component exists ───────────────────────────────────
  test("(a) app-local-models-list custom element is mounted", async ({
    page,
  }) => {
    // This is the RED→GREEN gate: before the component is created the custom
    // element is absent from the DOM; after wiring it into ai-role-rows it is present.
    await expect(page.locator("app-local-models-list")).toBeAttached();
  });

  // ── (b) — one row per brainModels() entry ───────────────────────────────
  test("(b) renders one row per list_brain_models entry", async ({ page }) => {
    await expect(page.locator(".brain-model-row")).toHaveCount(2);
  });

  // ── (c) — "In use" badge on the selected model ──────────────────────────
  test('(c) shows "In use" badge on the selected model', async ({ page }) => {
    await expect(page.getByText("In use")).toBeVisible();
  });

  // ── (d) — Download button on the not-downloaded model ───────────────────
  test("(d) shows Download button on the not-downloaded model", async ({
    page,
  }) => {
    await expect(page.getByRole("button", { name: "Download" })).toBeVisible();
  });
});
