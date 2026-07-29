import { expect, test } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

const CONFIG_WITH_GROUNDING_OFF = {
  providerId: "claude_code",
  anthropicModel: "claude-opus-4-8",
  ollamaBaseUrl: "http://localhost:11434",
  ollamaModel: "llama3.1",
  claudeBinary: "claude",
  captureSystemAudio: true,
  vadEnabled: true,
  keepHiresMasters: false,
  diarizeOthers: true,
  voiceprintEnabled: false,
  aecEnabled: false,
  postAecEnabled: false,
  modelSize: "small",
  liveAsrEngine: "whisper",
  brainIdleTimeoutSecs: 300,
  brainReadyTimeoutSecs: 90,
  brainHardCapSecs: 180,
  voiceTrigger: false,
  onboarded: true,
  sharingChoiceMade: true,
  noteStyle: "standard",
  notesMode: "enhance",
  autoOrganize: false,
  noteLanguage: "auto",
  groundSummary: false,
  mcpRequireToken: true,
  lockRequireBiometric: true,
  relockOnScreenshare: true,
  cloudEgressConsented: false,
  brainBackend: "off",
  realtimeReactions: false,
  proactiveHintsEnabled: true,
  userMemoryEnabled: true,
  semanticSearchEnabled: true,
};

test.describe("Notes settings — local grounding toggle", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(
      page,
      {
        save_config: (args: { config: unknown }) => {
          (
            window as unknown as { __savedGroundingConfig?: unknown }
          ).__savedGroundingConfig = args.config;
          return null;
        },
      },
      { get_config: CONFIG_WITH_GROUNDING_OFF },
    );
    await page.goto("/settings");
    await page.getByText("Notes", { exact: true }).first().click();
  });

  test("live-loads OFF and saves each explicit choice without enabling voiceprints", async ({
    page,
  }) => {
    const toggle = page.getByRole("checkbox", {
      name: /Flag potentially unsupported claims/,
    });

    await expect(toggle).toBeVisible({ timeout: 10_000 });
    await expect(toggle).not.toBeChecked();

    await toggle.check();
    await expect(toggle).toBeChecked();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (
              window as unknown as {
                __savedGroundingConfig?: {
                  groundSummary?: boolean;
                  voiceprintEnabled?: boolean;
                };
              }
            ).__savedGroundingConfig,
        ),
      )
      .toMatchObject({ groundSummary: true, voiceprintEnabled: false });

    await toggle.uncheck();
    await expect(toggle).not.toBeChecked();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (
              window as unknown as {
                __savedGroundingConfig?: {
                  groundSummary?: boolean;
                  voiceprintEnabled?: boolean;
                };
              }
            ).__savedGroundingConfig,
        ),
      )
      .toMatchObject({ groundSummary: false, voiceprintEnabled: false });
  });

  test("describes a local uncalibrated review cue, not proof", async ({ page }) => {
    await expect(
      page.getByText(/conservative thresholds are not yet calibrated/),
    ).toBeVisible();
    await expect(
      page.getByText(/not proof that a claim is true or false/),
    ).toBeVisible();
    await expect(
      page.getByText(/runs locally and adds no cloud request/),
    ).toBeVisible();
  });
});
