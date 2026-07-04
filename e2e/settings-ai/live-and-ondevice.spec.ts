import { test, expect } from "@playwright/test";
import { mockTauri } from "./mock-invoke";

/**
 * Integration spec for DuringMeetingsBlockComponent +
 * OnDeviceIntelligenceBlockComponent (Task 5).
 *
 * RED contract:
 *   (a)–(b) The custom elements don't exist in the DOM before the components
 *       are created → `toBeAttached()` fails = RED.
 *   (c) The assistant + proactive-hints checkboxes are not present before
 *       Task 5 → `toBeAttached()` fails = RED.
 *   (d) The semantic-search toggle + Re-index button are not present before
 *       Task 5 → fails = RED.
 *   (e) The cloud-consent warning doesn't render before the component →
 *       `toBeVisible()` fails = RED.
 */
test.describe("during-meetings-block + on-device-intelligence-block", () => {
  test.beforeEach(async ({ page }) => {
    await mockTauri(page);
    await page.goto("/settings");
    await page.getByText("AI & Models").first().click();
  });

  // ── (a) — app-during-meetings-block custom element is mounted ────────────
  test("(a) app-during-meetings-block custom element is in the DOM", async ({
    page,
  }) => {
    await expect(page.locator("app-during-meetings-block")).toBeAttached();
  });

  // ── (b) — app-on-device-intelligence-block custom element is mounted ─────
  test("(b) app-on-device-intelligence-block custom element is in the DOM", async ({
    page,
  }) => {
    await expect(page.locator("app-on-device-intelligence-block")).toBeAttached();
  });

  // ── (c) — assistant + proactive-hints toggles render ────────────────────
  test("(c) in-meeting assistant and proactive-hints checkboxes render", async ({
    page,
  }) => {
    await expect(
      page.locator('input[formcontrolname="realtimeReactions"]'),
    ).toBeAttached();
    await expect(
      page.locator('input[formcontrolname="proactiveHintsEnabled"]'),
    ).toBeAttached();
  });

  // ── (d) — semantic-search toggle + Re-index notes button render ──────────
  test("(d) semantic-search toggle and Re-index notes button render", async ({
    page,
  }) => {
    await expect(
      page.locator('input[formcontrolname="semanticSearchEnabled"]'),
    ).toBeAttached();
    await expect(
      page.getByRole("button", { name: "Re-index notes" }),
    ).toBeVisible();
  });
});

// ── (e) — cloud-consent warning appears when conditions are met ───────────
test("(e) cloud-consent warning appears when realtime on + live target is cloud + not consented", async ({
  page,
}) => {
  // cloudEgressConsented:false forces liveTargetIsCloud=true
  // (claude_code provider → cloud) and cloudConsented=false → banner visible.
  await mockTauri(page, {
    get_config: () =>
      Object.assign(
        {},
        {
          providerId: "claude_code",
          vaultPath: "/demo",
          vaultSubfolder: "Meetings",
          whisperModelPath: null,
          language: null,
          anthropicModel: "claude-opus-4-8",
          providerModel: "",
          providerEffort: "",
          ollamaBaseUrl: "http://localhost:11434",
          ollamaModel: "llama3.1:8b",
          claudeBinary: "claude",
          inputDevice: null,
          captureSystemAudio: true,
          vadEnabled: true,
          keepHiresMasters: false,
          diarizeOthers: true,
          aecEnabled: false,
          postAecEnabled: false,
          modelSize: "large-v3",
          voiceTrigger: true,
          onboarded: true,
          noteStyle: "structured",
          autoOrganize: true,
          noteLanguage: "en",
          mcpRequireToken: true,
          lockRequireBiometric: true,
          relockOnScreenshare: true,
          cloudEgressConsented: false,
          brainBackend: "cloud",
          realtimeReactions: true,
          brainModelId: "bielik-11b",
          brainModelPath: null,
          semanticSearchEnabled: true,
          webSearchEnabled: false,
          webSearchConsented: false,
          claudeCodeInheritEnv: false,
          gatewayBaseUrl: "",
          gatewayModel: "",
          proactiveHintsEnabled: true,
          roleNotesConnection: "",
          roleNotesModel: "",
          roleNotesEffort: "",
          roleAskConnection: "",
          roleAskModel: "",
          roleAskEffort: "",
          roleLiveConnection: "",
          roleLiveModel: "",
          roleLiveEffort: "",
        },
      ),
  });
  await page.goto("/settings");
  await page.getByText("AI & Models").first().click();
  // The banner text that appears when consent is needed.
  await expect(
    page.getByText(/sends live meeting context to your provider/),
  ).toBeVisible({ timeout: 10_000 });
});
