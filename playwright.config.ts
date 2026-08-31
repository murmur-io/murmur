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
    // `retain-on-failure` RECORDS every test and throws the recording away when it passes, which
    // is pure cost on a suite that is ~99% passing. Measured on the FULL suite, same machine,
    // `--workers=2`: 7.5 min -> 7.1 / 7.2 min across two runs, i.e. about 5%.
    //
    // Do not trust the bigger number a small sample gives you here: a 14-test subset showed
    // webkit 14.0 s -> 11.2 s and chromium 11.0 s -> 8.7 s, which reads as 20% — but per-test
    // recording overhead is a larger share of a short run than of the whole suite, and
    // extrapolating it overstated the win four-fold. 5% is the honest figure.
    //
    // CI therefore records on the RETRY instead: it runs `retries: 2`, so anything that actually
    // fails is re-run and the second attempt carries a full trace and video. Nothing that fails
    // arrives without artifacts; they just come from attempt #2.
    //
    // LOCAL keeps `retain-on-failure`, deliberately: `retries: 0` there, so `on-first-retry` would
    // capture NOTHING — and the harness's verifier reads exactly these artifacts. Screenshots stay
    // `only-on-failure` everywhere; they are captured after the fact and cost nothing on a pass.
    trace: process.env["CI"] ? "on-first-retry" : "retain-on-failure",
    screenshot: "only-on-failure",
    video: process.env["CI"] ? "on-first-retry" : "retain-on-failure",
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
