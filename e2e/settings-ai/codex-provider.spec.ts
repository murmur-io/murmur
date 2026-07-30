import { expect, test } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

test("Codex can be selected globally and per feature with its catalog and branded card", async ({
  page,
}) => {
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(String(error)));

  // Start with a concrete Claude model so the engine switch exercises the stale-model repair
  // rather than the already-valid explicit "Default (provider's pick)" option.
  await page.addInitScript(() => {
    (
      window as unknown as {
        __demoConfig?: Record<string, unknown>;
      }
    ).__demoConfig = { providerModel: "claude-opus-4-8" };
  });

  await mockTauri(
    page,
    {
      save_config: (args: { config: unknown }) => {
        const w = window as unknown as {
          __savedCodexConfig?: unknown;
          __demoConfig?: Record<string, unknown>;
        };
        w.__savedCodexConfig = args.config;
        w.__demoConfig = Object.assign({}, w.__demoConfig, args.config);
        return null;
      },
      provider_statuses: () => {
        const w = window as unknown as { __codexProbeCalls?: number };
        w.__codexProbeCalls = (w.__codexProbeCalls ?? 0) + 1;
        return [
          { id: "claude_code", available: true },
          { id: "codex_cli", available: true },
          { id: "anthropic", available: true },
          { id: "ollama", available: true },
          { id: "gateway", available: false, reason: "Not configured" },
        ];
      },
    },
    {
      get_egress_ledger: {
        totalCalls: 1,
        totalTokens: 812,
        byModel: [{ model: "gpt-5.6-terra", calls: 1, tokens: 812 }],
        byDay: [{ day: "2026-07-29", tokens: 812 }],
        totalRedactions: { email: 1, card: 0, phone: 0, name: 2 },
        recent: [
          {
            ts: 1785319200,
            providerId: "codex_cli",
            destination: "codex_cli (OpenAI Codex CLI)",
            modelServed: "gpt-5.6-terra",
            totalTokens: 812,
            redactions: { email: 1, card: 0, phone: 0, name: 2 },
          },
        ],
      },
    },
  );

  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();

  const setup = page.locator("app-ai-setup-block");
  const engine = setup.locator('select[formcontrolname="providerId"]');
  await expect(engine.locator('option[value="codex_cli"]')).toHaveText("Codex");
  await engine.selectOption("codex_cli");

  const defaultModel = setup.locator('select[formcontrolname="providerModel"]');
  await expect(defaultModel.locator("[data-provider-default]")).toHaveText(
    "Default (provider's pick)",
  );
  await expect(defaultModel).toHaveValue("");
  await expect(defaultModel.locator(":checked")).toHaveText(
    "Default (provider's pick)",
  );
  await expect(page.locator("app-ai-privacy-strip")).toContainText(
    "Codex → OpenAI (via the Codex CLI)",
  );
  await expect(defaultModel.locator('option[value="gpt-5.6-sol"]')).toHaveCount(
    1,
  );
  await expect(
    defaultModel.locator('option[value="gpt-5.6-terra"]'),
  ).toHaveCount(1);
  await expect(
    defaultModel.locator('option[value="gpt-5.6-luna"]'),
  ).toHaveCount(1);
  await defaultModel.selectOption("gpt-5.6-terra");

  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __savedCodexConfig?: {
                providerId?: string;
                providerModel?: string;
              };
            }
          ).__savedCodexConfig,
      ),
    )
    .toMatchObject({
      providerId: "codex_cli",
      providerModel: "gpt-5.6-terra",
    });

  await page.getByRole("button", { name: /Advanced/ }).click();
  await page.getByRole("button", { name: /Show all engines/ }).click();
  for (const provider of [
    "claude_code",
    "codex_cli",
    "anthropic",
    "ollama",
    "gateway",
  ]) {
    await expect(
      page.locator(`[data-provider-icon="${provider}"]`),
    ).toHaveCount(1);
  }
  const codexIconBackground = await page
    .locator('[data-provider-icon="codex_cli"]')
    .evaluate((element) => getComputedStyle(element).backgroundColor);
  const claudeIconBackground = await page
    .locator('[data-provider-icon="claude_code"]')
    .evaluate((element) => getComputedStyle(element).backgroundColor);
  expect(codexIconBackground).not.toBe("rgba(0, 0, 0, 0)");
  expect(codexIconBackground).not.toBe(claudeIconBackground);
  const codexCard = page
    .locator("app-ai-connection-card")
    .filter({ has: page.locator('[data-provider-icon="codex_cli"]') });
  await expect(codexCard).toContainText("Codex");
  await expect(codexCard).toContainText("Cloud — redacted first");
  await codexCard.getByRole("button", { name: /Configure/ }).click();
  await expect(codexCard).toContainText("Uses your installed Codex CLI");
  await expect(codexCard).toContainText(
    "Test checks the installed CLI file and its private sign-in file without contacting OpenAI",
  );
  await expect(codexCard).toContainText("codex login");
  await expect(codexCard).toContainText(
    "Codex tools, workspace reads, web search and connectors are disabled",
  );
  const probeCallsBefore = await page.evaluate(
    () =>
      (window as unknown as { __codexProbeCalls?: number }).__codexProbeCalls ??
      0,
  );
  await codexCard.getByRole("button", { name: "Test" }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __codexProbeCalls?: number })
            .__codexProbeCalls ?? 0,
      ),
    )
    .toBeGreaterThan(probeCallsBefore);

  await page.getByRole("button", { name: /Customize per feature/ }).click();
  const notesRow = page.locator('[data-role="notes"]');
  // These names are bound dynamically (`[formControlName]="row.connCtrl"`), so
  // Angular does not retain a literal formcontrolname attribute in the DOM.
  await notesRow.locator("select").first().selectOption("codex_cli");
  const notesModel = notesRow.locator("select").nth(1);
  await expect(notesModel).toHaveValue("");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __savedCodexConfig?: {
                roleNotesConnection?: string;
                roleNotesModel?: string;
              };
            }
          ).__savedCodexConfig,
      ),
    )
    .toMatchObject({
      roleNotesConnection: "codex_cli",
      roleNotesModel: "",
    });
  await expect(notesModel.locator('option[value="gpt-5.6-luna"]')).toHaveCount(
    1,
  );
  await notesModel.selectOption("gpt-5.6-luna");

  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __savedCodexConfig?: {
                roleNotesConnection?: string;
                roleNotesModel?: string;
              };
            }
          ).__savedCodexConfig,
      ),
    )
    .toMatchObject({
      roleNotesConnection: "codex_cli",
      roleNotesModel: "gpt-5.6-luna",
    });

  await page.goto("/analytics");
  const ledger = page.locator("app-egress-ledger");
  await expect(ledger).toBeVisible({ timeout: 10_000 });
  await expect(ledger).toContainText("codex_cli (OpenAI Codex CLI)");
  await expect(ledger).toContainText("gpt-5.6-terra");

  expect(pageErrors).toEqual([]);
});

test("a delayed Codex catalog cannot overwrite a newer engine selection", async ({
  page,
}) => {
  await page.addInitScript(() => {
    (
      window as unknown as {
        __demoConfig?: Record<string, unknown>;
      }
    ).__demoConfig = { providerModel: "gpt-5.6-terra" };
  });
  await mockTauri(page, {
    list_models: (args: { connection: string }) => {
      if (args.connection === "codex_cli") {
        return new Promise((resolve) =>
          setTimeout(
            () => resolve(["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]),
            200,
          ),
        );
      }
      if (args.connection === "anthropic") {
        return ["claude-opus-4-8", "claude-sonnet-4-6", "claude-haiku-4-5"];
      }
      return [];
    },
  });

  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();
  const setup = page.locator("app-ai-setup-block");
  const engine = setup.locator('select[formcontrolname="providerId"]');
  await engine.selectOption("codex_cli");
  await engine.selectOption("anthropic");

  const model = setup.locator('select[formcontrolname="providerModel"]');
  await expect(engine).toHaveValue("anthropic");
  await expect(model).toHaveValue("");
  await page.waitForTimeout(250);
  await expect(engine).toHaveValue("anthropic");
  await expect(model).toHaveValue("");
});

test("an empty Codex catalog clears a foreign default-engine model", async ({
  page,
}) => {
  await page.addInitScript(() => {
    (
      window as unknown as {
        __demoConfig?: Record<string, unknown>;
      }
    ).__demoConfig = { providerModel: "claude-opus-4-8" };
  });
  await mockTauri(page, {
    list_models: () => [],
  });

  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();
  const setup = page.locator("app-ai-setup-block");
  await setup
    .locator('select[formcontrolname="providerId"]')
    .selectOption("codex_cli");
  await expect(
    setup.locator('input[formcontrolname="providerModel"]'),
  ).toHaveValue("");
});

test("a failed Codex catalog request preserves the stored model id", async ({
  page,
}) => {
  await page.addInitScript(() => {
    (
      window as unknown as {
        __demoConfig?: Record<string, unknown>;
      }
    ).__demoConfig = { providerModel: "claude-opus-4-8" };
  });
  await mockTauri(page, {
    list_models: () => {
      throw new Error("synthetic catalog outage");
    },
  });

  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();
  const setup = page.locator("app-ai-setup-block");
  await setup
    .locator('select[formcontrolname="providerId"]')
    .selectOption("codex_cli");
  await expect(
    setup.locator('input[formcontrolname="providerModel"]'),
  ).toHaveValue("claude-opus-4-8");
});

test("a loaded Codex role clears and persists a foreign Claude model id", async ({
  page,
}) => {
  await page.addInitScript(() => {
    (
      window as unknown as {
        __demoConfig?: Record<string, unknown>;
      }
    ).__demoConfig = {
      roleNotesConnection: "codex_cli",
      roleNotesModel: "claude-opus-4-8",
    };
  });
  await mockTauri(page, {
    save_config: (args: { config: unknown }) => {
      const w = window as unknown as {
        __savedCodexRoleRepair?: unknown;
        __demoConfig?: Record<string, unknown>;
      };
      w.__savedCodexRoleRepair = args.config;
      w.__demoConfig = Object.assign({}, w.__demoConfig, args.config);
      return null;
    },
  });

  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();
  const notesRow = page.locator('[data-role="notes"]');
  await expect(notesRow).toBeVisible();
  await expect(notesRow.locator("select").first()).toHaveValue("codex_cli");
  await expect(notesRow.locator("select").nth(1)).toHaveValue("");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __savedCodexRoleRepair?: {
                roleNotesConnection?: string;
                roleNotesModel?: string;
              };
            }
          ).__savedCodexRoleRepair,
      ),
    )
    .toMatchObject({
      roleNotesConnection: "codex_cli",
      roleNotesModel: "",
    });
});

test("Codex cloud-consent copy names OpenAI's cloud", async ({ page }) => {
  await page.addInitScript(() => {
    (
      window as unknown as {
        __demoConfig?: Record<string, unknown>;
      }
    ).__demoConfig = { providerId: "codex_cli" };
  });
  await mockTauri(page, {
    model_present: () => true,
    start_recording: () => ({
      meetingId: "m-codex-consent",
      startedAt: "2026-07-29T09:00:00Z",
    }),
    stop_recording: () =>
      Promise.reject(
        "provider unavailable: [cloud-consent] synthetic consent fixture",
      ),
  });

  await page.goto("/record");
  await page.locator("button.start-btn").click();
  await expect(page.locator(".rec-topbar")).toBeVisible({ timeout: 10_000 });
  await page.locator("button.stop-btn").click();
  const consent = page.locator(".banner.cloud-consent");
  await expect(consent).toBeVisible({ timeout: 10_000 });
  await expect(consent).toContainText("OpenAI's cloud");
  process.stderr.write(
    "MURMUR_CODEX_E2E_EXECUTED selection_catalog_icons_persistence_consent=true\n",
  );
});
