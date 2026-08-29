import { expect, test, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * "Convert to note" refusals must name the ONE thing the user has to do next.
 *
 * Every refusal on this path used to be anonymous, so `ErrorCopyService`'s deny-by-default
 * rendered "Couldn’t convert this meeting. Please try again." for all of them. The refusal that
 * actually fired in production — a shared note in the meeting's folder — took a SQLCipher session
 * against the user's own database to identify, because the app could not say it.
 *
 * The wire strings below are the real shape: `<AppError variant tag>: [code] <developer prose>`.
 * The prose half must never reach the screen; the code half selects the sentence.
 */
async function refuseConvertWith(page: Page, wire: string): Promise<void> {
  await mockTauri(
    page,
    {},
    { audit_reminder_suggestions: [], __convertRefusal: wire },
  );
  // The mock's constant channel cannot throw, so the rejection is installed page-side over it.
  await page.addInitScript((message: string) => {
    const install = () => {
      const internals = (window as unknown as { __TAURI_INTERNALS__?: { invoke?: unknown } })
        .__TAURI_INTERNALS__;
      if (!internals || typeof internals.invoke !== "function") {
        return false;
      }
      const original = internals.invoke as (cmd: string, args: unknown) => Promise<unknown>;
      internals.invoke = (cmd: string, args: unknown) =>
        cmd === "convert_meeting_to_note"
          ? Promise.reject(message)
          : original(cmd, args);
      return true;
    };
    if (!install()) {
      const timer = setInterval(() => {
        if (install()) clearInterval(timer);
      }, 5);
    }
  }, wire);
}

async function convert(page: Page): Promise<void> {
  await page.goto("/meeting/m-atlas-roadmap");
  const button = page
    .getByTestId("meeting-command-bar")
    .getByRole("button", { name: "Convert to note", exact: true });
  await expect(button).toBeVisible({ timeout: 10_000 });
  await button.click();
}

test("a shared note in the way is named, not hidden behind “try again”", async ({ page }) => {
  await refuseConvertWith(
    page,
    "provider unavailable: [share-active] revoke this note's shares before filing this converted note",
  );
  await convert(page);

  await expect(page.locator(".toast.is-danger .toast-msg")).toHaveText(
    "Couldn’t convert this meeting — This meeting’s note is shared. Revoke its share first, then convert again.",
  );
});

test("a missing transcript says so instead of asking for a retry that cannot work", async ({
  page,
}) => {
  await refuseConvertWith(
    page,
    "invalid argument: [convert-no-transcript] this meeting has no transcript to convert",
  );
  await convert(page);

  await expect(page.locator(".toast.is-danger .toast-msg")).toHaveText(
    "Couldn’t convert this meeting — This recording has no transcript yet, so there’s nothing to turn into a note.",
  );
});

test("deny-by-default still holds: an un-coded failure never renders its wire string", async ({
  page,
}) => {
  await refuseConvertWith(
    page,
    "storage error: account-session mutex poisoned at src-tauri/src/state.rs:118",
  );
  await convert(page);

  const toast = page.locator(".toast.is-danger .toast-msg");
  await expect(toast).toHaveText("Couldn’t convert this meeting. Please try again.");
  // The control arm — without it the two tests above would pass even if the copy layer had been
  // replaced by "render whatever Rust sent".
  await expect(toast).not.toContainText("mutex");
  await expect(toast).not.toContainText("state.rs");
});
