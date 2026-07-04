import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright FE harness for Murmur's Angular UI. Boots `ng serve` on a DEDICATED
 * test port (4210) — NOT the dev-convention :1420, which a running dev app owns —
 * so the suite always exercises THIS worktree's build in isolation. Specs mock the
 * Tauri IPC (see e2e/settings-ai/mock-invoke.ts), so no Rust core is needed.
 */
const PORT = 4210;

export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: `http://localhost:${PORT}`,
    colorScheme: "dark",
    trace: "on-first-retry",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: `ng serve --port ${PORT}`,
    url: `http://localhost:${PORT}`,
    reuseExistingServer: true,
    timeout: 180_000,
  },
});
