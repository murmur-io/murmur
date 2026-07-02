# Learnings — adversarial-verifier

## Recurring patterns
<!-- Curated, binding. Prepended to every dispatch. Keep ≤ ~20 bullets. -->

- **A green build/lint proves almost nothing at the FE↔BE seam.** Every shipped bug below passed
  a green `ng build` + `cargo test`. Live-reproduce: drive `:1420` with a mocked
  `window.__TAURI_INTERNALS__.invoke`, or boot the dev app and watch `/tmp/murmur-dev.log` for a
  clean start (no abort).
- **RED before GREEN.** A bug fix needs a regression that fails on the OLD code and passes on the
  new. A test that passes against unpatched code didn't capture the bug.
- **Hunt the failure modes this project actually ships:**
  1. **Seal content-loss** — keyed dedup destroying non-first rows on encrypt.
  2. **Sealed-content leak** — a read/asset path returning sealed data un-gated, especially
     `audio_path` reaching `convertFileSrc`/the `asset:` protocol (bypasses every backend command).
  3. **macOS FFI abort** — an unrecognized-selector `NSException` crossing FFI → abort at launch.
  4. **NG0600** — a signal written in an `effect()` without `{ allowSignalWrites: true }`.
  5. **Import-cycle `ɵcmp`** — mutually-recursive standalone components missing `forwardRef`.
  6. **Opacity bleed** — a popover/modal using the frosted `.card` instead of `--surface-overlay`.
  7. **Prod-only CSP style break** — reproduce in WebKit + the real `style-src` CSP, not Chromium;
     a styled *shell* screenshot is a FALSE PASS — judge the route CONTENT and read the console.
- **Own PASS/FAIL; never self-certify on the author's behalf.** A test that green-washes a real bug
  is the worst outcome. A reviewer told to "find gaps" will invent some — flag only
  correctness/leak/loss-affecting findings.
- **Pin verify/review agents to the absolute MAIN repo path** — `isolation:worktree` shifts cwd and
  a verifier can false-FAIL on the wrong tree.
- **Say what needs a real Mac.** Mic capture, live ScreenCaptureKit, Touch ID, lock-at-rest,
  screen-share auto-relock only truly verify on a *signed* build — a green unit test is not proof.
- **Playwright defaults colorScheme LIGHT** and a mock field typo poisons judge rounds — eyeball the
  PNG yourself before trusting a verdict.

## Run journal
<!-- Append-only, newest first. -->

### [2026-07-02 seed] Distilled from agentic-workflow.md + memory notes
- **Pattern:** the 7 shipped-and-caught failure modes + the verify-live / RED-before-GREEN discipline.
- **Caught by:** operator (seeding the loop).
- **Lesson:** the bullets above; full detail in `.claude/rules/agentic-workflow.md`.
- **Status:** distilled (2026-07-02)
