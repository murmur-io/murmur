import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * `note_style` is a free-form `String` on the Rust side (`AppConfigDto.note_style`,
 * src-tauri/src/commands.rs) with NO enum validation — only an empty string is
 * normalized (to "standard"); any other stored value passes through unvalidated.
 * The demo mock (scripts/screenshots/mock-tauri.js) sets `noteStyle: "structured"`,
 * a value outside the four hardcoded <option>s in
 * settings-notes-section.component.html — so this is a real, live repro, not a
 * contrived one.
 *
 * RED (pre-fix): the native <select> had no matching <option> for "structured" so
 * it rendered with NOTHING visibly selected, while the help text's
 * `@switch`/`@default` silently showed the "standard" balanced-summary copy —
 * mislabeling the actual stored value with no indication anything was wrong.
 *
 * GREEN (post-fix): an extra <option> surfaces the raw unrecognized value so the
 * select always shows a real selection, and the help text's `@default` says the
 * value is unrecognized instead of lying that it's "standard".
 */
test.describe("Notes settings — Summary style select with an unrecognized stored value", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page);
    await page.goto("/settings");
    await page.getByText("Notes", { exact: true }).first().click();
  });

  test("the select shows a real visible selection, not blank", async ({
    page,
  }) => {
    const select = page.locator('select[formcontrolname="noteStyle"]');
    await expect(select).toBeVisible({ timeout: 10_000 });

    // The demo mock's noteStyle is "structured" — confirm it actually loaded
    // (guards the assertion below from passing vacuously against a stale form).
    await expect
      .poll(async () => select.inputValue())
      .toBe("structured");

    // The select must have a matching <option> for the current value — i.e. a
    // real visible selection, not the browser's "no option selected" blank state.
    const selectedOptionText = await select
      .locator("option:checked")
      .textContent();
    expect(selectedOptionText).not.toBeNull();
    expect(selectedOptionText!.trim().length).toBeGreaterThan(0);
  });

  test("the help text does not mislabel the unrecognized value as standard", async ({
    page,
  }) => {
    const select = page.locator('select[formcontrolname="noteStyle"]');
    await expect(select).toBeVisible({ timeout: 10_000 });
    await expect.poll(async () => select.inputValue()).toBe("structured");

    // Pre-fix this text appeared unconditionally for any unrecognized value.
    await expect(
      page.getByText(
        "A balanced summary, key points and action items — good for most",
      ),
    ).not.toBeVisible();
  });
});
