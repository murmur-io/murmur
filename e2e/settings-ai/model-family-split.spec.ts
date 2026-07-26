import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Pins ModelEffortPickerComponent's LANGUAGE-FAMILY split after it was
 * decoupled from the vendor product name.
 *
 * The split used to be `name.toLowerCase().includes("bielik")`; it is now
 * `!languages.includes("multi")` — registry DATA rather than a name sniff.
 *
 * (a) EQUIVALENCE — drives the picker with the SIX REAL `reason::BRAIN_MODELS`
 *     rows (ids, names, `languages`, classes and sizes copied verbatim from
 *     `src-tauri/src/reason.rs`) and asserts the two families come out exactly
 *     as the old name rule produced them:
 *
 *       Multilingual : qwen3-1.7b (light) · qwen3-4b-instruct-2507 + qwen3-14b (heavy)
 *       Polish-native: bielik-1.5b-v3 (light) · bielik-4.5b-v3 + bielik-11b-v3 (heavy)
 *
 * (b) THE DECOUPLING ITSELF — a Polish-native model that is NOT called
 *     "Bielik" must land in the Polish lane. This FAILS on the old name-sniff
 *     (it would be classified Multilingual) and passes on the language rule.
 */

/** The six live registry rows, verbatim from `reason::BRAIN_MODELS`. */
const REGISTRY = [
  {
    id: "qwen3-1.7b",
    name: "Qwen3 1.7B (light · multilingual)",
    class: "light",
    approxSizeBytes: 1_100_000_000,
    minRamGb: 4,
    languages: ["en", "multi", "pl"],
    arch: "qwen3",
    filename: "Qwen_Qwen3-1.7B-Q4_K_M.gguf",
    url: "https://example.com/qwen3-1.7b.gguf",
    downloaded: true,
    fitsRam: true,
    selected: false,
  },
  {
    id: "bielik-1.5b-v3",
    name: "Bielik 1.5B v3 (light · Polish-native)",
    class: "light",
    approxSizeBytes: 1_650_000_000,
    minRamGb: 4,
    languages: ["pl", "en"],
    arch: "llama",
    filename: "Bielik-1.5B-v3.0-Instruct.Q8_0.gguf",
    url: "https://example.com/bielik-1.5b.gguf",
    downloaded: true,
    fitsRam: true,
    selected: false,
  },
  {
    id: "qwen3-4b-instruct-2507",
    name: "Qwen3 4B Instruct 2507 (heavy · multilingual)",
    class: "heavy",
    approxSizeBytes: 2_500_000_000,
    minRamGb: 6,
    languages: ["en", "multi", "pl"],
    arch: "qwen3",
    filename: "Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
    url: "https://example.com/qwen3-4b.gguf",
    downloaded: true,
    fitsRam: true,
    selected: false,
  },
  {
    id: "bielik-4.5b-v3",
    name: "Bielik 4.5B v3 (heavy · Polish-native)",
    class: "heavy",
    approxSizeBytes: 4_900_000_000,
    minRamGb: 10,
    languages: ["pl", "en"],
    arch: "llama",
    filename: "Bielik-4.5B-v3.0-Instruct.Q8_0.gguf",
    url: "https://example.com/bielik-4.5b.gguf",
    downloaded: true,
    fitsRam: true,
    selected: false,
  },
  {
    id: "bielik-11b-v3",
    name: "Bielik 11B v3 (heavy · Polish-native)",
    class: "heavy",
    approxSizeBytes: 7_215_545_057,
    minRamGb: 10,
    languages: ["pl", "en"],
    arch: "llama",
    filename: "Bielik-11B-v3.0-Instruct.Q4_K_M.gguf",
    url: "https://example.com/bielik-11b.gguf",
    downloaded: true,
    fitsRam: true,
    selected: false,
  },
  {
    id: "qwen3-14b",
    name: "Qwen3 14B (heavy · multilingual/English)",
    class: "heavy",
    approxSizeBytes: 9_663_676_416,
    minRamGb: 14,
    languages: ["en", "multi"],
    arch: "qwen3",
    filename: "Qwen_Qwen3-14B-Q4_K_M.gguf",
    url: "https://example.com/qwen3-14b.gguf",
    downloaded: true,
    fitsRam: true,
    selected: false,
  },
];

/** A Polish-native model with NO "bielik" in its name — the decoupling probe. */
const UNNAMED_POLISH = {
  id: "pllum-8b",
  name: "PLLuM 8B Instruct (heavy · Polish-native)",
  class: "heavy",
  approxSizeBytes: 5_400_000_000,
  minRamGb: 12,
  languages: ["pl", "en"],
  arch: "llama",
  filename: "PLLuM-8B-Instruct.Q4_K_M.gguf",
  url: "https://example.com/pllum-8b.gguf",
  downloaded: true,
  fitsRam: true,
  selected: false,
};

/** Settings → AI & Models → ⚙ Advanced → Engines → "On this Mac" → Configure. */
async function openEffortPicker(page: import("@playwright/test").Page) {
  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();
  await page.getByRole("button", { name: /Advanced/ }).click();
  await page
    .locator("app-brain-engine-card")
    .getByRole("button", { name: /Configure/ })
    .click();
  // `toBeAttached`, NOT `toBeVisible`: the picker's :host is `display: contents`,
  // so it generates no box and Playwright would call it invisible.
  await expect(page.locator("app-model-effort-picker")).toBeAttached();
  await expect(page.locator("app-model-effort-picker .picker")).toBeVisible();
}

const picker = (page: import("@playwright/test").Page) =>
  page.locator("app-model-effort-picker");

test.describe("model-effort-picker — language family split", () => {
  test("(a) the six real registry rows split exactly as the old name rule did", async ({
    page,
  }) => {
    await mockTauri(page, {}, { list_brain_models: REGISTRY });
    await openEffortPicker(page);
    const p = picker(page);

    // Nothing is selected → the family falls back to Multilingual, and the
    // slider sits on that family's SMALLEST heavy model.
    await expect(p.locator(".seg button.on")).toHaveText(/Multilingual/);
    await expect(p.locator(".r-name")).toHaveText(
      "Qwen3 4B Instruct 2507 (heavy · multilingual)",
    );
    // Exactly the TWO multilingual heavy models feed the effort slider.
    await expect(p.locator(".ticks .tick")).toHaveCount(2);
    await expect(p.locator('input[type="range"]')).toHaveAttribute("max", "1");
    // …and the automatic light model is the multilingual one.
    await expect(p.locator(".auto-note")).toContainText(
      "Qwen3 1.7B (light · multilingual)",
    );

    // Switch lanes: only the three Bielik rows live here, never a Qwen.
    await p.getByRole("button", { name: /Polish-native/ }).click();
    await expect(p.locator(".seg button.on")).toHaveText(/Polish-native/);
    await expect(p.locator(".r-name")).toHaveText(
      "Bielik 4.5B v3 (heavy · Polish-native)",
    );
    await expect(p.locator(".ticks .tick")).toHaveCount(2);
    await expect(p.locator('input[type="range"]')).toHaveAttribute("max", "1");
    await expect(p.locator(".auto-note")).toContainText(
      "Bielik 1.5B v3 (light · Polish-native)",
    );
  });

  test('(b) a Polish-native model NOT named "Bielik" still lands in the Polish lane', async ({
    page,
  }) => {
    await mockTauri(
      page,
      {},
      {
        // The ONLY heavy models: one multilingual, one Polish-native whose name
        // carries no vendor keyword. The old `name.includes("bielik")` rule
        // would file PLLuM under Multilingual.
        list_brain_models: [
          ...REGISTRY.filter((m) => m.class === "light"),
          REGISTRY.find((m) => m.id === "qwen3-4b-instruct-2507"),
          UNNAMED_POLISH,
        ],
      },
    );
    await openEffortPicker(page);
    const p = picker(page);

    // Multilingual must NOT have absorbed the Polish model — one heavy only,
    // so the effort slider is not rendered at all (`heavyModels().length > 1`).
    await expect(p.locator(".seg button.on")).toHaveText(/Multilingual/);
    await expect(p.locator(".r-name")).toHaveText(
      "Qwen3 4B Instruct 2507 (heavy · multilingual)",
    );
    await expect(p.locator('input[type="range"]')).toHaveCount(0);

    // The Polish lane resolves to PLLuM, paired with the Polish light model.
    await p.getByRole("button", { name: /Polish-native/ }).click();
    await expect(p.locator(".r-name")).toHaveText(
      "PLLuM 8B Instruct (heavy · Polish-native)",
    );
    await expect(p.locator(".auto-note")).toContainText(
      "Bielik 1.5B v3 (light · Polish-native)",
    );
  });
});
