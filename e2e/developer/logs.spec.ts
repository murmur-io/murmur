import { expect, test, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * Developer mode + the Logs view.
 *
 * Two things are worth pinning here, and neither is provable by reading the
 * component: that the tools stay HIDDEN until the user asks for them (the
 * toggle defaults off, and a default that silently drifts to on is exactly the
 * kind of regression nobody notices), and that a log window actually renders as
 * formatted rows rather than as one wall of text.
 *
 * The IPC is mocked, so this is the UI oracle only — it says nothing about what
 * the Rust side reads off disk. That half is covered by `applog`'s parser tests
 * and its camelCase wire assertion; the mock here is deliberately written in
 * the SAME camelCase the backend serializes, which a hand-written mock can
 * define but never verify (angular-zoneless T6).
 */
/**
 * Route `read_app_log` by the requested session, the way the backend does.
 * The override runs page-side and is serialized, so its fixtures must be inline
 * (no closure over test-scope constants).
 */
async function boot(page: Page): Promise<void> {
  await mockTauri(
    page,
    {
      read_app_log: (args: any) =>
        args?.session === "previous"
          ? {
              session: "previous",
              path: "/Users/x/Library/Application Support/MeetNotes-dev/murmur.prev.log",
              exists: false,
              sizeBytes: 0,
              truncated: false,
              entries: [],
            }
          : {
              session: "current",
              path: "/Users/x/Library/Application Support/MeetNotes-dev/murmur.log",
              exists: true,
              sizeBytes: 4096,
              truncated: false,
              entries: [
                {
                  seq: 0,
                  timestamp: "2026-09-01T10:11:12.123456Z",
                  level: "INFO",
                  target: "murmur::pipeline",
                  message: "stage complete count=3",
                },
                {
                  seq: 1,
                  timestamp: "2026-09-01T10:11:13.500000Z",
                  level: "WARN",
                  target: "murmur::audio",
                  message: "system audio helper unavailable",
                },
                {
                  seq: 2,
                  timestamp: "2026-09-01T10:11:14.750000Z",
                  level: "ERROR",
                  target: "panic",
                  message: 'location="src/lib.rs:12" message="boom"',
                },
              ],
            },
    },
    {},
  );
}

function developerNav(page: Page) {
  return page.getByRole("navigation", { name: "Developer mode" });
}

test("developer mode is off by default — the sidebar shows no developer group", async ({
  page,
}) => {
  await boot(page);
  await page.goto("/record");
  await expect(page.getByRole("navigation", { name: "Primary navigation" })).toBeVisible();
  await expect(developerNav(page)).toHaveCount(0);
});

test("turning developer mode on reveals the sidebar group and its Logs entry", async ({
  page,
}) => {
  await boot(page);
  await page.goto("/settings");
  await page.getByText("Developer").first().click();

  const section = page.locator("app-settings-developer-section");
  const toggle = section.getByRole("checkbox", { name: "Developer mode" });
  await expect(toggle).not.toBeChecked();

  await toggle.check();
  await expect(toggle).toBeChecked();

  // The shell sidebar reads the same root signal, so it lights up immediately —
  // no reload, no navigation.
  const nav = developerNav(page);
  await expect(nav).toBeVisible();
  await expect(nav.getByRole("link", { name: "Logs" })).toBeVisible();
});

test("the log renders as formatted rows, filterable by level and by text", async ({
  page,
}) => {
  await boot(page);
  await page.goto("/settings");
  await page.getByText("Developer").first().click();
  await page
    .locator("app-settings-developer-section")
    .getByRole("checkbox", { name: "Developer mode" })
    .check();

  await developerNav(page).getByRole("link", { name: "Logs" }).click();

  const list = page.getByRole("log", { name: "Application log" });
  await expect(list.locator(".log-row")).toHaveCount(3);

  // Formatting: the level, the target and the message are separate columns, and
  // an error row carries the error class the palette hangs off.
  const error = list.locator(".log-row.is-error");
  await expect(error).toHaveCount(1);
  await expect(error.locator(".log-level")).toHaveText("ERROR");
  await expect(error.locator(".log-target")).toHaveText("panic");

  // Level filter: Errors leaves exactly the one error row.
  await page.getByRole("button", { name: /^Errors/ }).click();
  await expect(list.locator(".log-row")).toHaveCount(1);

  // Text filter, back on All.
  await page.getByRole("button", { name: /^All/ }).click();
  await page.getByRole("searchbox", { name: "Filter log messages" }).fill("audio");
  await expect(list.locator(".log-row")).toHaveCount(1);
  await expect(list.locator(".log-row .log-target")).toHaveText("murmur::audio");
});

test("a never-written previous session reads as 'no previous session', not an error", async ({
  page,
}) => {
  await boot(page);
  await page.goto("/settings");
  await page.getByText("Developer").first().click();
  await page
    .locator("app-settings-developer-section")
    .getByRole("checkbox", { name: "Developer mode" })
    .check();
  await developerNav(page).getByRole("link", { name: "Logs" }).click();

  await page.getByRole("button", { name: "Previous session" }).click();

  await expect(page.getByText("No previous session")).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
});
