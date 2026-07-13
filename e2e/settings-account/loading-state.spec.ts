import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Settings › Account — the "Loading account…" state must resolve, never hang.
 *
 * Bug: `SettingsAccountSectionComponent`'s template used to gate its entire
 * signed-out/signed-in UI behind `@if (status(); as st) { … } @else { Loading
 * account… }`. Angular's `@if(x; as y)` treats a resolved `null`/falsy value
 * IDENTICALLY to "not yet resolved" — there was no signal distinguishing
 * "still pending" from "settled, but falsy". A benign resolved-`null`
 * `account_status` response (e.g. an unmocked command falling through to a
 * generic default, as the demo/screenshot mock used to do) left the section
 * stuck on "Loading account…" forever, with no sign-in button and no error.
 *
 * Fix: a dedicated `loaded` signal, set `true` in `reload()`'s `finally`
 * block regardless of outcome — the template now gates "Loading account…" on
 * `!loaded()`, so a settled-but-falsy status correctly falls through to the
 * signed-out "Create or sign in to a sharing account" affordance.
 *
 * This test overrides `account_status` to resolve `null` directly (the exact
 * settled-but-falsy shape the bug hinged on) — RED against the pre-fix
 * template (stuck on "Loading account…", sign-in button never appears),
 * GREEN after the fix.
 */

async function openAccountSection(
  page: import("@playwright/test").Page,
): Promise<void> {
  await page.goto("/settings");
  await page.getByRole("button", { name: "Account" }).first().click();
  await expect(
    page.locator("app-settings-account-section"),
  ).toBeVisible({ timeout: 10_000 });
}

test.describe("Settings › Account — resolved-null status (mocked IPC)", () => {
  test("shows the sign-in affordance, not a stuck 'Loading account…'", async ({
    page,
  }) => {
    const errors: string[] = [];
    page.on("console", (m) => {
      if (m.type() === "error") {
        errors.push(m.text());
      }
    });

    await mockTauri(page, {
      // The exact settled-but-falsy shape that used to defeat the
      // `@if (status(); as st)` truthiness gate.
      account_status: () => null,
    });
    await openAccountSection(page);

    const section = page.locator("app-settings-account-section");

    // The permanent-spinner regression: "Loading account…" must NOT still be
    // showing once the (resolved) IPC call has settled.
    await expect(section.getByText("Loading account…")).toHaveCount(0, {
      timeout: 10_000,
    });

    // The signed-out sign-in affordance must be visible instead.
    await expect(
      section.getByRole("button", {
        name: "Create or sign in to a sharing account",
      }),
    ).toBeVisible();

    expect(errors).toEqual([]);
  });
});
