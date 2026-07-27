import { test, expect } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/**
 * THE HIGHEST-RISK SEAM IN P3: the cloud-consent hand-off between Rust prose and an Angular regex.
 *
 * Before this change, `record.component.ts` decided whether to show the "Allow" banner by
 * regex-matching the backend's ENGLISH SENTENCE:
 *
 *     needsCloudConsent = /cloud egress not consented/i.test(store.error())
 *
 * That made an ordinary de-jargoning edit — rewording a Rust error string — a silent break for
 * every cloud user's first note: the Allow banner would stop rendering, and the raw backend string
 * would surface in the red error banner instead. There is no test that would have caught it, and
 * no compiler that could.
 *
 * The contract is now the stable `[cloud-consent]` CODE (`src-tauri/src/errcode.rs::CLOUD_CONSENT`,
 * emitted by `summarize::make_provider_resolved`), matched by `RecorderStore.errorCode()`.
 *
 * RED CONTRACT (why this is a real regression test, not a restatement):
 *   The fixture below rejects `stop_recording` with the NEW wire string, which contains the code
 *   and NOT the old sentence. Against the PRE-CHANGE frontend the prose regex finds no match, so
 *   `needsCloudConsent` is false, the Allow banner never renders, and the raw string leaks into the
 *   danger banner — both assertions fail. Against the post-change frontend it renders.
 *
 *   Verify that by hand with:
 *     needsCloudConsent = computed(() => /cloud egress not consented/i.test(this.store.error() ?? ""))
 *   which reproduces the old behaviour and turns this spec RED.
 */

/*
 * THE WIRE STRING UNDER TEST — `AppError::Unavailable(errcode::tag(CLOUD_CONSENT, …)).to_string()`:
 *
 *   "provider unavailable: [cloud-consent] this provider sends meeting content off-device;
 *    grant one-time consent before using it"
 *
 * It is INLINED at each override site rather than hoisted into a `const`, and must stay that way:
 * `mockTauri` serializes every override with `Function.prototype.toString()` and replays it
 * PAGE-SIDE, so an override that closes over a test-scope binding throws a `ReferenceError` in the
 * page instead of rejecting — the spec would then fail for a reason that has nothing to do with
 * the consent flow.
 */

test.describe("Record — the cloud-consent banner is driven by the CODE, not by Rust prose", () => {
  test("a [cloud-consent] rejection renders the Allow banner instead of a raw error", async ({
    page,
  }) => {
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({
        meetingId: "m-consent",
        startedAt: "2026-07-27T09:00:00Z",
      }),
      // Inlined on purpose — see the note above the describe block.
      stop_recording: () =>
        Promise.reject(
          "provider unavailable: [cloud-consent] this provider sends meeting content off-device; grant one-time consent before using it",
        ),
    });

    await page.goto("/record");
    await page.locator("button.start-btn").click();
    await expect(page.locator(".rec-topbar")).toBeVisible({ timeout: 10_000 });
    await page.locator("button.stop-btn").click();

    // The consent banner — the whole point of classifying this failure rather than showing it.
    const consent = page.locator(".banner.cloud-consent");
    await expect(consent).toBeVisible({ timeout: 10_000 });
    await expect(consent).toContainText(/Cloud processing isn't enabled/i);
    await expect(consent.locator("button.btn-primary")).toHaveText(
      /Allow & finish note/i,
    );

    // …and the generic danger banner must NOT be the thing that rendered.
    await expect(page.locator(".banner.is-danger")).toHaveCount(0);

    // The raw wire string must not reach the screen in ANY form — neither the code nor the
    // developer prose after it.
    const body = await page.locator("app-record").innerText();
    expect(body).not.toContain("[cloud-consent]");
    expect(body).not.toMatch(/provider unavailable/i);
    expect(body).not.toMatch(/off-device/i);
  });

  test("an UNRELATED failure still renders the plain error banner, never the Allow banner", async ({
    page,
  }) => {
    // Deny-by-default in the other direction: an un-coded backend failure must not accidentally
    // classify as consent (the old regex could, for any message mentioning consent), and must not
    // render its own text.
    await mockTauri(page, {
      model_present: () => true,
      start_recording: () => ({
        meetingId: "m-boom",
        startedAt: "2026-07-27T09:00:00Z",
      }),
      stop_recording: () =>
        Promise.reject(
          "transcription error: brain sidecar stdin missing; HKDF expand failed",
        ),
    });

    await page.goto("/record");
    await page.locator("button.start-btn").click();
    await expect(page.locator(".rec-topbar")).toBeVisible({ timeout: 10_000 });
    await page.locator("button.stop-btn").click();

    await expect(page.locator(".banner.is-danger")).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator(".banner.cloud-consent")).toHaveCount(0);

    // The never-show vocabulary in that message is exactly why deny-by-default exists.
    const body = await page.locator("app-record").innerText();
    expect(body).not.toMatch(/sidecar/i);
    expect(body).not.toMatch(/HKDF/i);
    expect(body).not.toMatch(/stdin/i);
  });
});
