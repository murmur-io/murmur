import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { mockTauri } from "./mock-invoke";

const RECOMMENDATION = JSON.parse(
  readFileSync(
    join(__dirname, "..", "fixtures", "whisper-recommendation.json"),
    "utf8",
  ),
);

/**
 * The transcription power picker in Settings.
 *
 * The FIRST test here is a regression for a real BLOCKER found by review: picking a
 * rung wrote the form control programmatically with `setValue()`, which does NOT
 * mark the form dirty — and the store's debounced auto-save is gated on
 * `form.dirty`. So on a PRISTINE Settings visit (the normal case) the rung moved on
 * screen and nothing was ever persisted. The picker looked like it worked.
 *
 * PROVING IT IS REALLY RED: remove `this.form.markAsDirty()` from
 * `settings-transcription-section.component.ts::onSizeChange` and the first test must
 * FAIL with zero recorded saves.
 */
async function boot(page: import("@playwright/test").Page) {
  await mockTauri(
    page,
    {
      save_config: (args: any) => {
        const w = window as any;
        w.__saves = w.__saves || [];
        w.__saves.push(args.config?.modelSize ?? null);
        w.__demoConfig = Object.assign({}, w.__demoConfig, args.config);
        return null;
      },
    },
    { whisper_recommendation: RECOMMENDATION },
  );
  await page.goto("/settings");
  await page.getByRole("button", { name: /Transcription/i }).first().click();
}

test("a rung picked on a PRISTINE form is actually saved", async ({ page }) => {
  await boot(page);

  const slider = page.getByRole("slider", {
    name: "Transcription model power",
  });
  await expect(slider).toBeVisible({ timeout: 10_000 });

  // Touch NOTHING else first — a pristine form is the case that silently lost the pick.
  await slider.fill("0");
  await slider.dispatchEvent("change");

  await expect
    .poll(async () => await page.evaluate(() => (window as any).__saves ?? []), {
      timeout: 10_000,
    })
    .toContain("base");
});

test("dragging across rungs commits once, and announces the rung by NAME", async ({
  page,
}) => {
  await boot(page);

  const slider = page.getByRole("slider", {
    name: "Transcription model power",
  });
  await expect(slider).toBeVisible({ timeout: 10_000 });

  // A drag is many `input` events and ONE `change`. Only the change may commit —
  // otherwise a three-rung drag writes the config three times.
  await slider.fill("0");
  await slider.fill("1");
  await slider.fill("2");
  await slider.dispatchEvent("change");

  await expect
    .poll(async () => await page.evaluate(() => (window as any).__saves ?? []), {
      timeout: 10_000,
    })
    .toEqual(["large-v3-turbo-q8_0"]);

  // Accessibility: the value announced is the human rung name, never the index.
  const text = await slider.getAttribute("aria-valuetext");
  expect(text).toBeTruthy();
  expect(text).not.toMatch(/^\d+$/);
});

test("the badge tracks the selection, and the reason makes no unsupported claim", async ({
  page,
}) => {
  await boot(page);

  const slider = page.getByRole("slider", {
    name: "Transcription model power",
  });
  await expect(slider).toBeVisible({ timeout: 10_000 });
  const badge = page.getByText("Recommended for this Mac");
  const card = page.locator("app-model-power");

  // The fixture recommends Sharp (`large-v3-turbo-q8_0`), rung index 2. The badge is a
  // claim about the CURRENT selection, so it must appear only when they agree.
  await slider.fill("2");
  await slider.dispatchEvent("change");
  await expect(badge).toBeVisible();

  await slider.fill("0");
  await slider.dispatchEvent("change");
  await expect(badge).toBeHidden();

  // The fixture's reason is `alreadyDownloaded`, whose whole point is that PRESENCE —
  // not RAM — decided it, and the machine is Apple Silicon. A RAM-causal sentence or a
  // chip-family claim here would be the exact dishonesty two P1 review rounds removed.
  await expect(card).not.toContainText(/\d+\s*GB of (RAM|memory)/i);
  await expect(card).not.toContainText(/Intel/i);
});
