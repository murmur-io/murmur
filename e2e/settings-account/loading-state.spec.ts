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

test("Settings sign-in and logout update the global session banner without focus or polling", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
  await mockTauri(page, {
    account_status: () => {
      const w = window as unknown as { __settingsAccount?: Record<string, unknown> };
      return w.__settingsAccount ?? {
        accountExpected: true,
        loggedIn: false,
        email: null,
        unlockedForSharing: false,
        shareConsented: true,
        serverConfigured: true,
        biometricUnlockAvailable: false,
      };
    },
    account_login: () => {
      const status = {
        accountExpected: true,
        loggedIn: true,
        email: "ada@example.com",
        unlockedForSharing: true,
        shareConsented: true,
        serverConfigured: true,
        biometricUnlockAvailable: true,
      };
      (window as unknown as { __settingsAccount?: Record<string, unknown> })
        .__settingsAccount = status;
      return status;
    },
    account_logout: () => {
      (window as unknown as { __settingsAccount?: Record<string, unknown> })
        .__settingsAccount = {
          accountExpected: true,
          loggedIn: false,
          email: null,
          unlockedForSharing: false,
          shareConsented: true,
          serverConfigured: true,
          biometricUnlockAvailable: false,
        };
      return null;
    },
  });
  await openAccountSection(page);

  const section = page.locator("app-settings-account-section");
  const banner = page.getByLabel("Sharing account status");
  await expect(banner).toContainText("Your sharing session ended");
  await section
    .getByRole("button", { name: "Create or sign in to a sharing account" })
    .click();
  const dialog = page.getByRole("dialog", { name: "Sharing account" });
  await dialog.getByRole("button", { name: "I already have one" }).click();
  await page.evaluate(() => {
    const host = document.querySelector("app-sharing-auth-flow");
    const angular = (window as unknown as {
      ng: { getComponent: (element: Element) => { completed: { subscribe: (callback: (payload: unknown) => void) => void } } };
    }).ng;
    const component = angular.getComponent(host!);
    const capture = window as unknown as {
      __authCompletionPayloads?: Array<{
        type: string;
        hasEmail: boolean;
        keys: string[];
      }>;
    };
    capture.__authCompletionPayloads = [];
    component.completed.subscribe((payload: unknown) => {
      capture.__authCompletionPayloads!.push({
        type: typeof payload,
        hasEmail:
          typeof payload === "object" &&
          payload !== null &&
          "email" in payload,
        keys:
          typeof payload === "object" && payload !== null
            ? Object.keys(payload)
            : [],
      });
    });
  });
  await dialog.getByLabel("Email").fill("ada@example.com");
  await dialog.getByLabel("Password").fill("correct horse battery staple");
  await dialog.getByRole("button", { name: "Sign in", exact: true }).click();

  await expect(banner).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as {
          __authCompletionPayloads?: Array<{
            type: string;
            hasEmail: boolean;
            keys: string[];
          }>;
        }).__authCompletionPayloads ?? [],
    ),
  ).toEqual([{ type: "undefined", hasEmail: false, keys: [] }]);
  await expect(section.getByText("ada@example.com")).toBeVisible();
  await section.getByRole("button", { name: "Sign out" }).click();

  await expect(banner).toContainText("Your sharing session ended");
  await expect(
    section.getByRole("button", { name: "Create or sign in to a sharing account" }),
  ).toBeVisible();
  expect(errors).toEqual([]);
});

test("Settings Touch ID unlock clears the global locked banner immediately", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      unlock_sharing_with_biometric: () => {
        const status = {
          accountExpected: true,
          loggedIn: true,
          email: "ada@example.com",
          unlockedForSharing: true,
          shareConsented: true,
          serverConfigured: true,
          biometricUnlockAvailable: true,
        };
        (window as unknown as { __settingsTouchId?: Record<string, unknown> })
          .__settingsTouchId = status;
        return status;
      },
      account_status: () =>
        (window as unknown as { __settingsTouchId?: Record<string, unknown> })
          .__settingsTouchId ?? {
          accountExpected: true,
          loggedIn: true,
          email: "ada@example.com",
          unlockedForSharing: false,
          shareConsented: true,
          serverConfigured: true,
          biometricUnlockAvailable: true,
        },
    },
  );
  await openAccountSection(page);

  const banner = page.getByLabel("Sharing account status");
  await expect(banner).toContainText("Sharing is locked");
  await page
    .locator("app-settings-account-section")
    .getByRole("button", { name: "Unlock with Touch ID" })
    .click();
  await expect(banner).toHaveCount(0);
  await expect(page.getByText("Ready to share this session")).toBeVisible();
});
