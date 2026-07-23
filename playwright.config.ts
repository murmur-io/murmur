import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright FE harness for Murmur's Angular UI. Every invocation receives a
 * process-private port and starts a fresh server from THIS worktree. Specs mock
 * the Tauri IPC (see e2e/settings-ai/mock-invoke.ts), so this remains the fast UI
 * oracle; a task that claims native/Tauri behavior needs a separate runtime check.
 */
const requestedPort = Number.parseInt(process.env["MURMUR_E2E_PORT"] ?? "", 10);
const PORT = Number.isInteger(requestedPort) && requestedPort >= 1024 && requestedPort <= 65535
  ? requestedPort
  : 42000 + (process.pid % 20000);

// Playwright reloads this config inside worker processes. Persist the port in
// the parent environment before workers are spawned, otherwise each worker's
// PID would derive a different baseURL from the web-server port.
process.env["MURMUR_E2E_PORT"] = String(PORT);

export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    colorScheme: "dark",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
  ],
  webServer: {
    // E2E consumes one immutable worktree snapshot. Disabling the dev watcher
    // avoids unnecessary FSEvents authority/file descriptors inside the
    // harness's loopback-only Seatbelt profile.
    command: `ng serve --watch=false --host 127.0.0.1 --port ${PORT}`,
    url: `http://127.0.0.1:${PORT}`,
    reuseExistingServer: false,
    timeout: 180_000,
  },
});
