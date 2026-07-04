import type { Page } from "@playwright/test";
import * as path from "path";

/**
 * The proven demo Tauri v2 mock (installs `window.__TAURI_INTERNALS__` with a full
 * command router + event plumbing that boots the real Angular app over fictional data).
 */
const BASE_MOCK = path.resolve(
  __dirname,
  "../../scripts/screenshots/mock-tauri.js",
);

/**
 * Boot the app under the demo Tauri mock, then override specific Tauri commands for
 * this test. Overrides run PAGE-SIDE (serialized to strings — they must be
 * self-contained: no closures over test-scope variables). Unknown commands fall
 * through to the demo mock's benign defaults, so the app always boots.
 *
 * @example
 *   await mockTauri(page, { brain_posture: () => "hybrid" });
 */
export async function mockTauri(
  page: Page,
  overrides: Record<string, (args: any) => unknown> = {},
): Promise<void> {
  await page.addInitScript({ path: BASE_MOCK });
  const serialized = Object.fromEntries(
    Object.entries(overrides).map(([k, v]) => [k, v.toString()]),
  );
  await page.addInitScript((ov: Record<string, string>) => {
    const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__;
    const orig = internals.invoke.bind(internals);
    const names = Object.keys(ov);
    internals.invoke = (cmd: string, args: unknown) => {
      if (names.includes(cmd)) {
        const fn = new Function("args", `return (${ov[cmd]})(args);`);
        return Promise.resolve(fn(args ?? {}));
      }
      return orig(cmd, args);
    };
  }, serialized);
}
