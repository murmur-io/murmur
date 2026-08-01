import { test, expect, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";
import { readFileSync } from "node:fs";
import { join } from "node:path";

/**
 * The ONE shipped incident class Murmur had no automated coverage for.
 *
 * # What shipped broken
 *
 * v0.5.0 rendered in the packaged app with the GLOBAL stylesheet working (nav, header, `.btn`,
 * `.card` — everything in `styles.css`, loaded via `<link>`) but EVERY component's encapsulated
 * styles missing: folder chips bare, meeting rows running title and date together, lists showing
 * raw bullets. It cost three failed fixes before the cause was found, and `ng serve` never
 * reproduced it, so a green `ng build` proved nothing.
 *
 * # The mechanism
 *
 * Angular emulated encapsulation injects each component's styles at RUNTIME as
 * `document.createElement("style")` nodes — there are no per-component `.css` files. At build
 * time Tauri stamps a nonce on inline `<style>` elements in `index.html` and appends
 * `'nonce-<perload>'` to the served `style-src` (`tauri-utils/html.rs` `inject_nonce_token` →
 * `tauri/src/manager/mod.rs` `replace_csp_nonce`). Per CSP3 §6.7.3.2, once `style-src` contains a
 * nonce or hash source, `'unsafe-inline'` is IGNORED — so Angular's runtime `<style>` nodes, which
 * carry no nonce, are refused. The `<link>` global sheet survives because it is `'self'`, not
 * inline.
 *
 * The fix, live in `src-tauri/tauri.conf.json`, is one line: `dangerousDisableAssetCspModification:
 * ["style-src"]` tells Tauri not to touch that directive, so the declared `'unsafe-inline'` stays
 * effective. This spec is what stops that line being removed again.
 *
 * # Why this is NOT a WebKit-only test
 *
 * The rule is spec-mandated, not an engine quirk: CSP3 §6.7.3.2 step 2 ("if source list contains a
 * nonce-source ... return No") applies to every conforming engine, and Chromium enforces it
 * identically. The bug was invisible in development for a much duller reason — `ng serve` sends no
 * `Content-Security-Policy` header at all, so there was never a nonce to conflict with. Supplying
 * the real header is the entire trick, and it works in both configured projects.
 *
 * # Why two tests
 *
 * `applies_component_styles_under_the_real_csp` is the regression guard. `blocks_component_styles_
 * when_style_src_is_not_exempted` is the control that keeps the guard honest: it simulates the
 * broken configuration and asserts the styles really are blocked. Without it, any future change
 * that stopped the nonce from mattering would leave the first test passing vacuously — a green
 * oracle that has quietly stopped testing anything is worse than no oracle.
 */

const TAURI_CONF = join(__dirname, "..", "..", "src-tauri", "tauri.conf.json");

/** A stand-in for the per-load value Tauri generates; only its PRESENCE changes CSP semantics. */
const NONCE = "cGxheXdyaWdodC1jc3AtdGVzdA==";

interface Security {
  csp: string;
  dangerousDisableAssetCspModification?: string[];
}

function security(): Security {
  return JSON.parse(readFileSync(TAURI_CONF, "utf8")).app.security as Security;
}

/**
 * Reproduce Tauri's build-time transform: append a nonce to `style-src` unless the directive is
 * exempted. Only `style-src` is nonced here — Tauri also nonces `script-src`, but doing that would
 * block Angular's own bundles and the app would never boot, turning a precise assertion about
 * stylesheets into an uninformative blank page.
 */
function servedCsp(exemptions: string[]): string {
  return security()
    .csp.split(";")
    .map((directive) => directive.trim())
    .filter(Boolean)
    .map((directive) =>
      directive.startsWith("style-src") && !exemptions.includes("style-src")
        ? `${directive} 'nonce-${NONCE}'`
        : directive,
    )
    .join("; ");
}

interface Rendered {
  styleCount: number;
  blockedCount: number;
  refusals: string[];
}

/** Boot the app with `csp` served on the document, and report what the stylesheets actually did. */
async function renderUnder(page: Page, csp: string): Promise<Rendered> {
  const refusals: string[] = [];
  page.on("console", (message) => {
    if (/Refused to apply (a|an inline) style/i.test(message.text())) {
      refusals.push(message.text());
    }
  });

  await page.route("**/*", async (route) => {
    if (route.request().resourceType() !== "document") {
      return route.continue();
    }
    const response = await route.fetch();
    await route.fulfill({
      response,
      headers: { ...response.headers(), "content-security-policy": csp },
    });
  });

  await mockTauri(page, { get_graph: () => ({ nodes: [], edges: [] }) });
  await page.goto("/graph");
  await page.locator("app-root").waitFor({ state: "attached" });
  // Component styles are injected as the components instantiate, not at document parse.
  await expect
    .poll(async () => page.locator("style").count(), { timeout: 10_000 })
    .toBeGreaterThan(0);

  const counts = await page.evaluate(() => {
    const styles = Array.from(document.querySelectorAll("style"));
    return {
      styleCount: styles.length,
      // A stylesheet refused by CSP is parsed but never attached: `sheet` stays null.
      blockedCount: styles.filter((element) => element.sheet === null).length,
    };
  });
  return { ...counts, refusals };
}

test.describe("packaged-webview CSP", () => {
  test("applies component styles under the real csp", async ({ page }) => {
    const exemptions = security().dangerousDisableAssetCspModification ?? [];
    expect(
      exemptions,
      "tauri.conf.json must keep style-src exempt from Tauri's nonce injection, or every " +
        "Angular component ships unstyled in the packaged app (the v0.5.0 incident)",
    ).toContain("style-src");

    const rendered = await renderUnder(page, servedCsp(exemptions));

    expect(
      rendered.styleCount,
      "no <style> elements were found at all — Angular stopped injecting component styles at " +
        "runtime, so this spec is no longer testing the thing it was written for",
    ).toBeGreaterThan(0);
    expect(
      rendered.blockedCount,
      `${rendered.blockedCount} of ${rendered.styleCount} component stylesheets were refused ` +
        `by CSP. Console: ${rendered.refusals.join(" | ") || "(none)"}`,
    ).toBe(0);
    expect(rendered.refusals, "CSP refused a stylesheet").toEqual([]);
  });

  test("blocks component styles when style-src is not exempted", async ({ page }) => {
    // The control. This is the configuration the app shipped with in v0.5.0.
    const rendered = await renderUnder(page, servedCsp([]));

    expect(
      rendered.styleCount,
      "the control needs <style> elements to be able to block",
    ).toBeGreaterThan(0);
    expect(
      rendered.blockedCount,
      "a nonce in style-src must make 'unsafe-inline' ineffective (CSP3 §6.7.3.2), so Angular's " +
        "un-nonced runtime <style> nodes must be refused. If this passes, the guard above has " +
        "stopped proving anything and the mechanism must be re-derived before trusting it.",
    ).toBeGreaterThan(0);
  });
});
