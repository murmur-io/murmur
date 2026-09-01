import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

const STORED_GLOSSARY = "Konnect = Connect, Kinect\nFastMCP = Fast MCP";
const UPDATED_GLOSSARY =
  "Kong Operator = KO\nMurmur Protocol = MeetNotes Protocol";

async function openNotesSettings(
  page: import("@playwright/test").Page,
): Promise<void> {
  await page.getByText("Notes", { exact: true }).first().click();
  await expect(page.getByLabel("Workspace glossary")).toBeVisible({
    timeout: 10_000,
  });
}

test("workspace glossary loads and saves only after blur, then survives a Settings remount", async ({
  page,
}) => {
  await mockTauri(page, {
    save_config: (args: any) => {
      const w = window as any;
      w.__glossarySaves = w.__glossarySaves || [];
      w.__glossarySaves.push(args.config?.glossary ?? null);
      w.__demoConfig = Object.assign({}, w.__demoConfig, args.config);
      return null;
    },
  });
  await page.addInitScript((glossary: string) => {
    (window as any).__demoConfig = Object.assign(
      {},
      (window as any).__demoConfig ?? {},
      { glossary },
    );
  }, STORED_GLOSSARY);

  await page.goto("/settings");
  await openNotesSettings(page);

  const textarea = page.getByLabel("Workspace glossary");
  await expect(textarea).toHaveValue(STORED_GLOSSARY);

  await textarea.fill(UPDATED_GLOSSARY);
  await page.waitForTimeout(650);
  expect(
    await page.evaluate(() => (window as any).__glossarySaves ?? []),
  ).toEqual([]);

  await textarea.blur();
  await expect
    .poll(
      async () =>
        await page.evaluate(() => (window as any).__glossarySaves ?? []),
      { timeout: 10_000 },
    )
    .toEqual([UPDATED_GLOSSARY]);

  await page.getByRole("button", { name: "Close settings" }).click();
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await openNotesSettings(page);
  await expect(page.getByLabel("Workspace glossary")).toHaveValue(
    UPDATED_GLOSSARY,
  );
});
