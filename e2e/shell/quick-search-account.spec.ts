import { expect, test, type Locator, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

const consoleErrors = new WeakMap<Page, string[]>();

test.beforeEach(({ page }) => {
  const errors: string[] = [];
  consoleErrors.set(page, errors);
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
});

test.afterEach(({ page }) => {
  expect(consoleErrors.get(page) ?? []).toEqual([]);
});

async function openPalette(page: Page): Promise<void> {
  await page
    .getByRole("navigation", { name: "Primary navigation" })
    .getByRole("button", { name: "Search", exact: true })
    .click();
  await expect(page.getByRole("dialog", { name: "Quick search" })).toBeVisible();
}

async function sampleSurface(locator: Locator): Promise<{
  background: string;
  alpha: number;
  backdrop: string;
  webkitBackdrop: string;
}> {
  return locator.evaluate((element) => {
    const sampleAlpha = (cssColor: string): number => {
      const canvas = document.createElement("canvas");
      canvas.width = 1;
      canvas.height = 1;
      const context = canvas.getContext("2d", { willReadFrequently: true });
      if (!context) return -1;

      context.clearRect(0, 0, 1, 1);
      context.fillStyle = "rgba(0, 0, 0, 0)";
      context.fillStyle = cssColor;
      context.fillRect(0, 0, 1, 1);
      return context.getImageData(0, 0, 1, 1).data[3] ?? -1;
    };
    const style = getComputedStyle(element);
    return {
      background: style.backgroundColor,
      alpha: sampleAlpha(style.backgroundColor),
      backdrop: style.getPropertyValue("backdrop-filter"),
      webkitBackdrop: style.getPropertyValue("-webkit-backdrop-filter"),
    };
  });
}

test("the surface oracle rejects translucent modern CSS colors", async ({ page }) => {
  await page.setContent('<div id="modern-color-probe"></div>');
  const probe = page.locator("#modern-color-probe");
  await probe.evaluate((element) => {
    (element as HTMLElement).style.backgroundColor =
      "color(srgb 1 1 1 / 0.5)";
  });

  const sample = await sampleSurface(probe);
  expect(sample.background).toMatch(/^color\(/);
  expect(sample.alpha).toBeGreaterThan(0);
  expect(sample.alpha).toBeLessThan(255);
});

test("literal Search autofocuses and opens the result through the tab service", async ({
  page,
}) => {
  await mockTauri(page);
  await page.goto("/library");
  await openPalette(page);

  const input = page.getByPlaceholder("Search meetings, notes, transcripts…");
  await expect(input).toBeFocused();
  await input.fill("Atlas");
  const result = page.getByRole("option", { name: /Project Atlas — Kickoff/ });
  await expect(result).toBeVisible();
  await input.press("Enter");

  await expect(page).toHaveURL(/\/meeting\/m-atlas-kickoff$/);
  const tabs = await page.evaluate(() =>
    JSON.parse(localStorage.getItem("murmur.tabs.v1") ?? "{}"),
  );
  expect(tabs.tabs).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ kind: "meeting", entityId: "m-atlas-kickoff" }),
    ]),
  );
});

test("Ask Brain uses the gated vault command and renders Markdown sources and citations", async ({
  page,
}) => {
  await mockTauri(page, {
    ask_vault: (args: unknown) => {
      const w = window as unknown as { __brainCalls?: unknown[] };
      (w.__brainCalls ??= []).push(args);
      return {
        answer: "The **launch decision** is May 30.",
        sources: [
          {
            meetingId: "m-atlas-roadmap",
            title: "Q2 Roadmap Planning",
            startedAt: "2026-08-13T09:00:00Z",
          },
        ],
        citations: ["[[Q2 Roadmap Planning]]"],
      };
    },
  });
  await page.goto("/library");
  await openPalette(page);

  await page.getByRole("tab", { name: "Ask Brain" }).click();
  const input = page.getByPlaceholder("Ask Brain about any meeting or note…");
  await input.fill("When do we launch?");
  await input.press("Enter");

  await expect(page.locator(".brain-answer strong")).toHaveText("launch decision");
  await expect(page.locator(".brain-answer app-sources")).toContainText("Q2 Roadmap Planning");
  await expect(page.locator(".brain-citations")).toContainText("[[Q2 Roadmap Planning]]");
  const calls = await page.evaluate(
    () => (window as unknown as { __brainCalls?: unknown[] }).__brainCalls ?? [],
  );
  expect(calls).toHaveLength(1);
  expect(calls[0]).toEqual(
    expect.objectContaining({ question: "When do we launch?", history: [] }),
  );
});

test("Ask Brain renders admitted org provenance through the Quick Search sink", async ({
  page,
}) => {
  await mockTauri(page, {
    ask_vault: () => ({
      answer: "The team chose **Project Cipher**.",
      sources: [
        {
          meetingId: "org:item-cipher",
          title: "Cipher rollout",
          startedAt: "2026-08-13T09:00:00Z",
          origin: {
            kind: "org",
            author: "Anna",
            orgItemId: "item-cipher",
          },
        },
      ],
      citations: ["[org · Anna] Cipher rollout"],
    }),
  });
  await page.goto("/library");
  await openPalette(page);
  await page.getByRole("tab", { name: "Ask Brain" }).click();
  const input = page.getByPlaceholder("Ask Brain about any meeting or note…");
  await input.fill("What did the team decide about Project Cipher?");
  await input.press("Enter");

  await expect(page.locator(".brain-answer strong")).toHaveText("Project Cipher");
  const orgSource = page.locator(".brain-answer .src-chip--org");
  await expect(orgSource).toContainText("Cipher rollout");
  await expect(orgSource).toContainText("Anna");
  await expect(orgSource).toHaveAttribute("href", "/org-item/item-cipher");
  await expect(page.locator(".brain-citations")).toContainText(
    "[org · Anna] Cipher rollout",
  );
});

const rejectedOrgGates = [
  "membership-admission",
  "context-disabled",
  "tombstoned",
  "authorization-revoked",
] as const;

for (const gate of rejectedOrgGates) {
  test(`Ask Brain renders no org plaintext when ${gate} rejects the read`, async ({
    page,
  }) => {
    const secret = `ORG_SECRET_${gate.toUpperCase().replaceAll("-", "_")}`;
    await mockTauri(page, {}, {
      ask_vault: {
        answer: "",
        sources: [],
        citations: [],
        // Malformed/excess backend keys are deliberately hostile. The Angular sink must consume
        // only the typed AskVaultResult lanes, which stay empty when the backend gate rejects.
        debugAnswer: `${secret} answer`,
        rejectedSource: { title: `${secret} source` },
        rejectedCitation: `${secret} citation`,
      },
    });
    await page.goto("/library");
    await openPalette(page);
    await page.getByRole("tab", { name: "Ask Brain" }).click();
    const input = page.getByPlaceholder("Ask Brain about any meeting or note…");
    await input.fill(`Can I read ${gate}?`);
    await input.press("Enter");

    const dialog = page.getByRole("dialog", { name: "Quick search" });
    await expect(dialog).not.toContainText(secret);
    await expect(page.locator(".brain-answer app-markdown .md-body")).toBeEmpty();
    await expect(page.locator(".brain-answer app-sources .src-chip")).toHaveCount(0);
    await expect(page.locator(".brain-citations")).toHaveCount(0);
  });
}

test("a late Brain answer cannot overwrite Search mode", async ({ page }) => {
  await mockTauri(page, {
    ask_vault: () =>
      new Promise((resolve) => {
        const w = window as unknown as { __resolveBrain?: () => void };
        w.__resolveBrain = () =>
          resolve({
            answer: "STALE BRAIN ANSWER",
            sources: [],
            citations: [],
          });
      }),
  });
  await page.goto("/library");
  await openPalette(page);
  await page.getByRole("tab", { name: "Ask Brain" }).click();
  const input = page.getByPlaceholder("Ask Brain about any meeting or note…");
  await input.fill("Hold this request");
  await input.press("Enter");
  await expect(page.getByRole("status", { name: "" })).toContainText("Brain is searching");

  await page.getByRole("tab", { name: "Search" }).click();
  await page.evaluate(() =>
    (window as unknown as { __resolveBrain?: () => void }).__resolveBrain?.(),
  );
  await expect(page.getByText("STALE BRAIN ANSWER")).toHaveCount(0);
  await expect(page.getByPlaceholder("Search meetings, notes, transcripts…")).toBeFocused();
});

test("a sealed Brain result renders no answer, source, or citation plaintext", async ({
  page,
}) => {
  await mockTauri(page, {
    ask_vault: () => ({ answer: "", sources: [], citations: [] }),
  });
  await page.goto("/library");
  await openPalette(page);
  await page.getByRole("tab", { name: "Ask Brain" }).click();
  const input = page.getByPlaceholder("Ask Brain about any meeting or note…");
  await expect(input).toBeEnabled();
  await input.fill("What is sealed?");
  await input.press("Enter");

  await expect(page.locator(".brain-answer app-markdown .md-body")).toBeEmpty();
  await expect(page.locator(".brain-answer app-sources .src-chip")).toHaveCount(0);
  await expect(page.locator(".brain-citations")).toHaveCount(0);
});

test("a lock invalidation scrubs Brain and rejects its late pre-lock answer", async ({
  page,
}) => {
  await mockTauri(page, {
    ask_vault: () =>
      new Promise((resolve) => {
        const w = window as unknown as { __resolvePreLockBrain?: () => void };
        w.__resolvePreLockBrain = () =>
          resolve({
            answer: "PRE-LOCK SECRET ANSWER",
            sources: [
              {
                meetingId: "m-secret",
                title: "PRE-LOCK SECRET SOURCE",
                startedAt: "2026-08-13T09:00:00Z",
              },
            ],
            citations: ["[[PRE-LOCK SECRET CITATION]]"],
          });
      }),
  });
  await page.goto("/library");
  await openPalette(page);
  await page.getByRole("tab", { name: "Ask Brain" }).click();
  const input = page.getByPlaceholder("Ask Brain about any meeting or note…");
  await expect(input).toBeEnabled();
  await input.fill("Held before lock");
  await input.press("Enter");
  await expect(page.getByText("Brain is searching your visible vault…")).toBeVisible();

  await page.evaluate(() => {
    (window as unknown as { __demoEmit: (event: string, payload: unknown) => void })
      .__demoEmit("murmur://ask-history-invalidated", null);
    (window as unknown as { __resolvePreLockBrain?: () => void })
      .__resolvePreLockBrain?.();
  });

  await expect(page.getByText(/PRE-LOCK SECRET/)).toHaveCount(0);
  await expect(page.locator(".brain-answer, .brain-citations, .src-chip")).toHaveCount(0);
});

test("Ask Brain cannot dispatch before the privacy-listener barrier", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      ask_vault: () => {
        const w = window as unknown as { __barrierAskCalls?: number };
        w.__barrierAskCalls = (w.__barrierAskCalls ?? 0) + 1;
        return { answer: "after barrier", sources: [], citations: [] };
      },
    },
    {},
    [],
    ["murmur://ask-history-invalidated"],
  );
  await page.goto("/library");
  await openPalette(page);
  const search = page.getByPlaceholder("Search meetings, notes, transcripts…");
  await search.fill("Wait for privacy");
  await page.getByRole("tab", { name: "Ask Brain" }).click();
  const input = page.getByPlaceholder("Ask Brain about any meeting or note…");
  await expect(input).toBeDisabled();
  expect(
    await page.evaluate(
      () => (window as unknown as { __barrierAskCalls?: number }).__barrierAskCalls ?? 0,
    ),
  ).toBe(0);

  await page.evaluate(() => {
    (window as unknown as { __demoReleaseEventListeners: (event: string) => void })
      .__demoReleaseEventListeners("murmur://ask-history-invalidated");
  });
  await expect(input).toBeEnabled();
  await input.press("Enter");
  await expect(page.getByText("after barrier")).toBeVisible();
});

test("local-only use stays silent", async ({
  page,
}) => {
  await mockTauri(
    page,
    {},
    {
      account_status: {
        accountExpected: false,
        loggedIn: false,
        email: null,
        unlockedForSharing: false,
        shareConsented: false,
        serverConfigured: false,
        biometricUnlockAvailable: false,
      },
    },
  );
  await page.goto("/library");
  await expect(page.getByLabel("Sharing account status")).toHaveCount(0);
});

test("expected account loss shows direct sign-in without visiting Settings", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      account_status: () => {
        const w = window as unknown as {
          __accountState?: Record<string, unknown>;
        };
        return w.__accountState ?? {
          accountExpected: true,
          loggedIn: true,
          email: "ada@example.com",
          unlockedForSharing: true,
          shareConsented: true,
          serverConfigured: true,
          biometricUnlockAvailable: true,
        };
      },
    },
  );
  await page.goto("/library");

  const banner = page.getByLabel("Sharing account status");
  await expect(banner).toHaveCount(0);
  await page.evaluate(() => {
    const w = window as unknown as {
      __accountState?: Record<string, unknown>;
    };
    w.__accountState = {
      accountExpected: true,
      loggedIn: false,
      email: null,
      unlockedForSharing: false,
      shareConsented: true,
      serverConfigured: true,
      biometricUnlockAvailable: false,
    };
    window.dispatchEvent(new Event("focus"));
  });
  await expect(banner).toContainText("Your sharing session ended");
  await expect(banner).toContainText("Your local notes still work");
  await openPalette(page);
  const paletteZ = await page.locator(".qs-scrim").evaluate((element) =>
    Number.parseInt(getComputedStyle(element).zIndex, 10),
  );
  const bannerZ = await banner.evaluate((element) =>
    Number.parseInt(getComputedStyle(element).zIndex, 10),
  );
  expect(paletteZ).toBeGreaterThan(bannerZ);
  await page.keyboard.press("Escape");
  await banner.getByRole("button", { name: "Sign in" }).click();

  const modal = page.getByRole("dialog", { name: "Sharing account" });
  await expect(modal.getByRole("heading", { name: "Sign in" })).toBeVisible();
  await expect(modal.getByLabel("Email")).toBeVisible();
  const modalSurface = await sampleSurface(modal.locator(".auth-modal"));
  expect(modalSurface.alpha).toBe(255);
  expect(modalSurface.backdrop).not.toContain("blur");
  expect(modalSurface.webkitBackdrop).not.toContain("blur");
});

test("a locked expected account offers Touch ID and clears the banner after unlock", async ({
  page,
}) => {
  await mockTauri(
    page,
    {
      unlock_sharing_with_biometric: (args: unknown) => {
        const w = window as unknown as { __touchIdCalls?: unknown[] };
        (w.__touchIdCalls ??= []).push(args);
        return {
          accountExpected: true,
          loggedIn: true,
          email: "ada@example.com",
          unlockedForSharing: true,
          shareConsented: true,
          serverConfigured: true,
          biometricUnlockAvailable: true,
        };
      },
    },
    {
      account_status: {
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
  await page.goto("/library");

  const banner = page.getByLabel("Sharing account status");
  await expect(banner).toContainText("Sharing is locked");
  await banner.getByRole("button", { name: "Touch ID" }).click();
  await expect(banner).toHaveCount(0);
  const calls = await page.evaluate(
    () => (window as unknown as { __touchIdCalls?: unknown[] }).__touchIdCalls ?? [],
  );
  expect(calls).toHaveLength(1);
});

test("a dismissed account notice resurfaces after a material session transition", async ({
  page,
}) => {
  await mockTauri(page, {
    account_status: () => {
      const w = window as unknown as {
        __dismissTransitionState?: Record<string, unknown>;
      };
      return w.__dismissTransitionState ?? {
        accountExpected: true,
        loggedIn: true,
        email: "ada@example.com",
        unlockedForSharing: false,
        shareConsented: true,
        serverConfigured: true,
        biometricUnlockAvailable: true,
      };
    },
  });
  await page.goto("/library");

  const banner = page.getByLabel("Sharing account status");
  await expect(banner).toContainText("Sharing is locked");
  await banner.getByRole("button", { name: "Dismiss for this session" }).click();
  await expect(banner).toHaveCount(0);

  // An identical focus refresh is the same observed status and stays dismissed.
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(banner).toHaveCount(0);

  // Losing the account session is a new material status and must get a fresh notice.
  await page.evaluate(() => {
    const w = window as unknown as {
      __dismissTransitionState?: Record<string, unknown>;
    };
    w.__dismissTransitionState = {
      accountExpected: true,
      loggedIn: false,
      email: null,
      unlockedForSharing: false,
      shareConsented: true,
      serverConfigured: true,
      biometricUnlockAvailable: false,
    };
    window.dispatchEvent(new Event("focus"));
  });
  await expect(banner).toContainText("Your sharing session ended");
});
