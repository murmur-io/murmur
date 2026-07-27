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
  // CI retries flaky tests; LOCAL stays 0 so a flake you just wrote fails loudly.
  //
  // This does NOT hide failures: a test that only passes on a retry is reported as
  // `flaky`, not `passed`, so the signal is preserved and quantified rather than
  // swallowed. What it removes is the all-or-nothing coupling — with `retries: 0`
  // a single timing-sensitive assertion anywhere in the ~330-test webkit project
  // fails the whole lane, and the shared macOS runner produces exactly that: three
  // consecutive runs of unrelated diffs (one Rust-only, one Python-only) each failed
  // a DIFFERENT webkit test, every one dying on an assertion timeout around 12-13s
  // rather than the 30s test timeout. That is runner timing noise, not a defect the
  // gate should be attributing to whichever PR happens to be passing through.
  //
  // A test that goes flaky PERSISTENTLY is still a bug to fix at the source — the
  // flaky count in the report is where to look for it.
  retries: process.env.CI ? 2 : 0,
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
