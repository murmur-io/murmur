import { expect, test } from "@playwright/test";

import { mockTauri } from "../settings-ai/mock-invoke";

test("an initial recovery-status failure is visible without leaking backend detail", async ({
  page,
}) => {
  await mockTauri(page, {
    get_filing_recovery_status: () =>
      Promise.reject(
        new Error(
          "database read failed at /Users/demo/Secret Vault/Private roadmap.md",
        ),
      ),
  });
  await page.goto("/");

  const unavailable = page.getByRole("alert", {
    name: "Filing recovery status unavailable",
  });
  await expect(unavailable).toBeVisible();
  await expect(unavailable).toContainText(
    "Murmur couldn’t check whether filing recovery needs attention. No recovery action was taken.",
  );
  await expect(unavailable).not.toContainText("Secret Vault");
  await expect(
    unavailable.getByRole("button", { name: "Check again", exact: true }),
  ).toBeVisible();
});

test("a clean status becomes a visible fixed-copy warning when a focus refresh fails", async ({
  page,
}) => {
  await mockTauri(page, {
    get_filing_recovery_status: () => {
      const target = window as unknown as { __failRecoveryRefresh?: boolean };
      if (target.__failRecoveryRefresh) {
        return Promise.reject(
          new Error(
            "focus refresh failed at /Users/demo/Secret Vault/Executive plan.md",
          ),
        );
      }
      return {
        degraded: false,
        attemptCount: 0,
        projectionCount: 0,
        sourceSnapshotCount: 0,
        issueToken: null,
        issueKind: null,
        canKeepExisting: false,
      };
    },
  });
  await page.goto("/");
  await expect(
    page.getByRole("alert", { name: /Filing recovery/ }),
  ).toHaveCount(0);

  await page.evaluate(() => {
    (
      window as unknown as { __failRecoveryRefresh?: boolean }
    ).__failRecoveryRefresh = true;
    window.dispatchEvent(new Event("focus"));
  });
  const unavailable = page.getByRole("alert", {
    name: "Filing recovery status unavailable",
  });
  await expect(unavailable).toBeVisible();
  await expect(unavailable).toContainText(
    "Murmur couldn’t check whether filing recovery needs attention. No recovery action was taken.",
  );
  await expect(unavailable).not.toContainText("Secret Vault");
});

test("degraded filing recovery stays visible, refreshes on focus, and keeps one external file only after confirmation", async ({
  page,
}) => {
  const runtimeErrors: string[] = [];
  page.on("pageerror", (error) => runtimeErrors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") {
      runtimeErrors.push(message.text());
    }
  });

  await page.setViewportSize({ width: 900, height: 720 });

  await mockTauri(page, {
    account_status: () => ({
      accountExpected: true,
      loggedIn: false,
      email: null,
      unlockedForSharing: false,
      shareConsented: false,
      serverConfigured: true,
      biometricUnlockAvailable: false,
    }),
    list_folders: () => [
      {
        id: "private-folder",
        name: "Private",
        parentId: null,
        noteCount: 1,
        locked: true,
        unlocked: true,
        kind: "meeting",
        children: [],
      },
    ],
    relock_all: () => null,
    get_filing_recovery_status: () => {
      const target = window as unknown as {
        __recoveryStatusCalls?: number;
        __recoveryStatus?: unknown;
      };
      target.__recoveryStatusCalls = (target.__recoveryStatusCalls ?? 0) + 1;
      target.__recoveryStatus ??= {
        degraded: true,
        attemptCount: 2,
        projectionCount: 1,
        sourceSnapshotCount: 1,
        issueToken: "opaque-issue-1",
        issueKind: "externalSourceReplacement",
        canKeepExisting: true,
      };
      return target.__recoveryStatus;
    },
    retry_filing_recovery: () => {
      const target = window as unknown as { __recoveryRetryCalls?: number };
      target.__recoveryRetryCalls = (target.__recoveryRetryCalls ?? 0) + 1;
      return Promise.reject(
        new Error(
          "private backend detail: /Users/demo/Secret Vault/Client roadmap.md",
        ),
      );
    },
    keep_existing_filing_file: (args: unknown) => {
      const target = window as unknown as {
        __keepExistingCalls?: unknown[];
        __recoveryStatus?: unknown;
      };
      (target.__keepExistingCalls ??= []).push(args);
      target.__recoveryStatus = {
        degraded: false,
        attemptCount: 0,
        projectionCount: 0,
        sourceSnapshotCount: 0,
        issueToken: null,
        issueKind: null,
        canKeepExisting: false,
      };
      return target.__recoveryStatus;
    },
  });
  await page.goto("/");

  const banner = page.getByRole("alert", {
    name: "Filing recovery needs attention",
  });
  await expect(banner).toBeVisible();
  await expect(banner).toContainText(
    "A source file changed while Murmur was filing it.",
  );
  await expect(banner).toContainText("2 attempts · 1 projection · 1 snapshot");
  await expect(banner).not.toContainText("opaque-issue-1");
  await expect(banner.getByRole("button", { name: /dismiss/i })).toHaveCount(0);
  // The "Lock all" footer control needs no preamble: the one sidebar shows the
  // Workspaces tree and its footer together, with no panel to open first.
  await page
    .getByRole("button", { name: "Re-seal all 1 unlocked folder now" })
    // The fixed recovery warning deliberately remains above shell chrome. Use
    // the actual Angular button handler to create a real ToastService notice;
    // the assertion below concerns the resulting narrow-layout stack.
    .evaluate((button: HTMLButtonElement) => button.click());
  const toast = page.getByRole("status").filter({
    hasText: "All folders re-sealed",
  });
  await expect(toast).toBeVisible();
  await page.setViewportSize({ width: 640, height: 720 });
  const accountBanner = page.getByRole("status", {
    name: "Sharing account status",
  });
  await expect(accountBanner).toBeVisible();
  const [accountBox, narrowRecoveryBox, toastBox] = await Promise.all([
    accountBanner.boundingBox(),
    banner.boundingBox(),
    toast.boundingBox(),
  ]);
  expect(accountBox).not.toBeNull();
  expect(narrowRecoveryBox).not.toBeNull();
  expect(toastBox).not.toBeNull();
  expect(rectanglesOverlap(accountBox!, narrowRecoveryBox!)).toBe(false);
  expect(rectanglesOverlap(narrowRecoveryBox!, toastBox!)).toBe(false);
  expect(
    await banner.evaluate((element) => {
      const probe = document.createElement("span");
      probe.style.color = "var(--surface-overlay)";
      document.body.append(probe);
      const expectedBackground = getComputedStyle(probe).color;
      probe.remove();
      const style = getComputedStyle(element);
      return {
        backgroundMatchesToken: style.backgroundColor === expectedBackground,
        backdropFilter:
          style.backdropFilter ||
          style.getPropertyValue("-webkit-backdrop-filter"),
      };
    }),
  ).toEqual({ backgroundMatchesToken: true, backdropFilter: "none" });

  const statusCallsBeforeFocus = await page.evaluate(
    () =>
      (window as unknown as { __recoveryStatusCalls?: number })
        .__recoveryStatusCalls ?? 0,
  );
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __recoveryStatusCalls?: number })
            .__recoveryStatusCalls ?? 0,
      ),
    )
    .toBeGreaterThan(statusCallsBeforeFocus);

  await banner.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(banner).toContainText(
    "Murmur couldn’t retry recovery safely. No existing vault file was changed.",
  );
  await expect(banner).not.toContainText("Secret Vault");

  await banner
    .getByRole("button", { name: "Keep existing vault file…", exact: true })
    .click();
  const confirmation = page.getByRole("alertdialog", {
    name: "Confirm keeping the existing vault file",
  });
  const destructiveConfirm = confirmation.getByRole("button", {
    name: "Keep existing and discard this recovery",
    exact: true,
  });
  await expect(destructiveConfirm).toBeFocused();
  await expect(confirmation).toContainText(
    "Recovery snapshots may be the only remaining copy",
  );
  await expect(confirmation).toContainText(
    "Existing vault files are never overwritten or deleted.",
  );
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __keepExistingCalls?: unknown[] })
          .__keepExistingCalls ?? [],
    ),
  ).toEqual([]);

  await page.keyboard.press("Escape");
  await expect(confirmation).toHaveCount(0);
  await expect(
    banner.getByRole("button", {
      name: "Keep existing vault file…",
      exact: true,
    }),
  ).toBeFocused();

  await banner
    .getByRole("button", { name: "Keep existing vault file…", exact: true })
    .click();
  await expect(confirmation).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { __recoveryStatus?: unknown }).__recoveryStatus = {
      degraded: true,
      attemptCount: 2,
      projectionCount: 1,
      sourceSnapshotCount: 1,
      issueToken: "opaque-issue-2",
      issueKind: "externalTargetOccupant",
      canKeepExisting: true,
    };
    window.dispatchEvent(new Event("focus"));
  });
  await expect(confirmation).toHaveCount(0);
  await expect(banner).toContainText(
    "A vault file now occupies a planned destination.",
  );

  await banner
    .getByRole("button", { name: "Keep existing vault file…", exact: true })
    .click();
  await expect(confirmation).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { __recoveryStatus?: unknown }).__recoveryStatus = {
      degraded: false,
      attemptCount: 0,
      projectionCount: 0,
      sourceSnapshotCount: 0,
      issueToken: null,
      issueKind: null,
      canKeepExisting: false,
    };
    window.dispatchEvent(new Event("focus"));
  });
  await expect(confirmation).toHaveCount(0);
  await expect(banner).toHaveCount(0);

  await page.evaluate(() => {
    (window as unknown as { __recoveryStatus?: unknown }).__recoveryStatus = {
      degraded: true,
      attemptCount: 1,
      projectionCount: 0,
      sourceSnapshotCount: 1,
      issueToken: "opaque-issue-3",
      issueKind: "externalSourceReplacement",
      canKeepExisting: true,
    };
    window.dispatchEvent(new Event("focus"));
  });
  await expect(banner).toBeVisible();
  await banner
    .getByRole("button", { name: "Keep existing vault file…", exact: true })
    .click();
  await expect(confirmation).toBeVisible();

  await confirmation
    .getByRole("button", {
      name: "Keep existing and discard this recovery",
      exact: true,
    })
    .click();
  await expect(confirmation).toHaveCount(0);
  await expect(banner).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __keepExistingCalls?: unknown[] })
          .__keepExistingCalls ?? [],
    ),
  ).toEqual([{ issueToken: "opaque-issue-3", confirmed: true }]);
  expect(runtimeErrors).toEqual([]);
});

function rectanglesOverlap(
  a: { x: number; y: number; width: number; height: number },
  b: { x: number; y: number; width: number; height: number },
): boolean {
  return (
    a.x < b.x + b.width &&
    a.x + a.width > b.x &&
    a.y < b.y + b.height &&
    a.y + a.height > b.y
  );
}
