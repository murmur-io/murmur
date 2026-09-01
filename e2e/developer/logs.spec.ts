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
                  raw: "2026-09-01T10:11:12.123456Z  INFO murmur::pipeline: stage complete count=3",
                },
                {
                  seq: 1,
                  timestamp: "2026-09-01T10:11:13.500000Z",
                  level: "WARN",
                  target: "murmur::audio",
                  message: "system audio helper unavailable",
                  raw: "2026-09-01T10:11:13.500000Z  WARN murmur::audio: system audio helper unavailable",
                },
                {
                  seq: 2,
                  timestamp: "2026-09-01T10:11:14.750000Z",
                  level: "ERROR",
                  target: "panic",
                  message: 'location="src/lib.rs:12" message="boom"',
                  raw: '2026-09-01T10:11:14.750000Z ERROR panic: location="src/lib.rs:12" message="boom"',
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

/** Turn developer mode on in Settings, then open the Logs view. */
async function openLogs(page: Page): Promise<void> {
  await page.goto("/settings");
  await page.getByText("Developer").first().click();
  await page
    .locator("app-settings-developer-section")
    .getByRole("checkbox", { name: "Developer mode" })
    .check();
  await developerNav(page).getByRole("link", { name: "Logs" }).click();
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
  await openLogs(page);

  const list = page.getByRole("log", { name: "Application log" });
  await expect(list.locator(".log-row")).toHaveCount(3);

  // Formatting: the level, the target and the message are separate columns, and
  // an error row carries the error class the palette hangs off.
  const error = list.locator(".log-entry.is-error");
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
  await openLogs(page);

  await page.getByRole("button", { name: "Previous", exact: true }).click();

  await expect(page.getByText("No previous session")).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("every action sits behind the ⋯ menu, and choosing one closes it", async ({
  page,
}) => {
  await boot(page);
  await openLogs(page);

  // The header carries ONE control, not a six-button toolbar.
  const trigger = page.getByRole("button", { name: "Log actions" });
  await expect(trigger).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Refresh" })).toHaveCount(0);

  await trigger.click();
  await expect(page.getByRole("menuitem", { name: "Refresh" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Copy" })).toBeVisible();
  await expect(
    page.getByRole("menuitem", { name: "Reveal in Finder" }),
  ).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Clear" })).toBeVisible();

  await page.getByRole("menuitem", { name: "Auto-refresh" }).click();

  // Choosing closes the menu; the header keeps reporting the lasting state.
  await expect(page.getByRole("menuitem", { name: "Refresh" })).toHaveCount(0);
  await expect(page.locator(".live-pill")).toBeVisible();

  // ...and the item now offers the reverse action.
  await trigger.click();
  await expect(
    page.getByRole("menuitem", { name: "Stop auto-refresh" }),
  ).toBeVisible();
});

test("Clear is offered for this session only — the previous one is evidence", async ({
  page,
}) => {
  await boot(page);
  await openLogs(page);

  await page.getByRole("button", { name: "Previous", exact: true }).click();
  await page.getByRole("button", { name: "Log actions" }).click();

  await expect(page.getByRole("menuitem", { name: "Refresh" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Clear" })).toHaveCount(0);
});

test("clicking a row expands it into the whole entry, and again collapses it", async ({
  page,
}) => {
  await boot(page);
  await openLogs(page);

  const list = page.getByRole("log", { name: "Application log" });
  const row = list
    .locator(".log-entry", { hasText: "system audio helper unavailable" })
    .locator(".log-row");
  await expect(row).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".row-detail")).toHaveCount(0);

  await row.click();

  await expect(row).toHaveAttribute("aria-expanded", "true");
  const detail = page.locator(".row-detail");
  await expect(detail).toBeVisible();
  // The detail carries what the row could only truncate: the exact UTC stamp,
  // the full target, and the line as the file has it.
  await expect(detail).toContainText("2026-09-01T10:11:13.500000Z");
  await expect(detail).toContainText("murmur::audio");
  await expect(detail.locator(".detail-raw")).toContainText(
    "WARN murmur::audio: system audio helper unavailable",
  );

  await row.click();
  await expect(row).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".row-detail")).toHaveCount(0);
});

test("an entry's structured fields are split out as key/value rows", async ({
  page,
}) => {
  await boot(page);
  await openLogs(page);

  await page
    .getByRole("log", { name: "Application log" })
    .locator(".log-entry.is-error .log-row")
    .click();

  const detail = page.locator(".row-detail");
  // `location="src/lib.rs:12" message="boom"` is two FIELDS, not prose — and the
  // panel shows their values unquoted.
  await expect(detail.locator(".detail-field-key")).toHaveText([
    "location",
    "message",
  ]);
  await expect(detail.locator(".detail-field-value").first()).toHaveText(
    "src/lib.rs:12",
  );
  await expect(detail.locator(".detail-field-value").last()).toHaveText("boom");
});

test("two entries can be open at once", async ({ page }) => {
  await boot(page);
  await openLogs(page);

  const list = page.getByRole("log", { name: "Application log" });
  await list.locator(".log-entry.is-warn .log-row").click();
  await list.locator(".log-entry.is-error .log-row").click();

  await expect(page.locator(".row-detail")).toHaveCount(2);
});
