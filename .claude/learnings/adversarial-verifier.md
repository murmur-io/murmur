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

### [2026-07-05 detail redesign — #194] a PASS on a tree that didn't build
- **Pattern:** A build workflow's verify phase ran WHILE the build phases had left the tree
  NON-BUILDING (a Split agent died mid-response + a syntax error cascaded), yet a structure-level
  "PASS" came back — the verdict was worthless because it never actually compiled the tree it judged.
- **Caught by:** operator (re-running `cargo test --lib` + `npx ng build` on the repaired final tree).
- **Lesson:** A verify verdict is only as good as the TREE STATE it ran against. Before hunting
  behavioural failure modes, first prove the thing COMPILES/BUILDS on the FINAL, settled tree — run
  the real gates yourself and paste the exact output you observed, never "gates green" secondhand. If
  a prior phase could have died/half-applied, assume the tree is broken until `ng build`/`cargo test`
  says otherwise.
- **Status:** journal

### [2026-07-04 PR#181 Murmur Brain] One adversarial pass MISSED 7 semantic-wiring gaps a multi-dimension Workflow caught
- **Pattern:** a single adversarial-verifier pass PASSed a LARGE multi-phase feature (registry / reasoner /
  postures / realtime whisper / local provider / FE) — yet a follow-up **6-dimension review Workflow**
  (wiring · invariants · spec-conformance · realtime · FE · robustness), each finding independently
  confirm-or-refuted, raised **22 confirmed** issues incl. a CRITICAL. The class a single pass
  systematically misses: (a) a config field WRITTEN by one command but READ via a DIFFERENT field by its
  consumer (`select` set `brain_model_id`; `light()` read `brain_light_model_id` → silent stub); (b) an
  invariant enforced at the TESTED site only (`derive_posture` checked Notes+Ask but not the Live axis →
  "Fully Local" over an egressing `@brain`); (c) dead code whose doc OVER-CLAIMS (`is_recording()` "drives
  the gate" with zero callers); (d) a preset command + a reactive form both writing the same keys → the
  stale form clobbers the preset on the next save (silent egress regression); (e) a worker-thread panic
  wedging a busy-flag; (f) a per-call tokio Runtime = thread leak; (g) a 416-on-complete-`.part` bricking
  download resume.
- **Caught by:** deep-review Workflow (ran AFTER adversarial-verifier PASS + lock-security PASS).
- **Lesson:** for a LARGE / multi-phase feature, one pass is NOT the gate — a multi-dimension deep-review
  Workflow is (ship-feature stage 4c). Even in your OWN single pass, actively hunt the wiring class: grep
  every new config field's PRODUCERS vs CONSUMERS (same field?); every new command's registration vs FE
  call; every "never X" invariant at ALL sites, not just the one with a test; every new helper's callers
  (0 callers + a doc claiming it "drives/gates" something = a red flag).
- **Status:** journal

### [2026-07-02 seed] Distilled from agentic-workflow.md + memory notes
- **Pattern:** the 7 shipped-and-caught failure modes + the verify-live / RED-before-GREEN discipline.
- **Caught by:** operator (seeding the loop).
- **Lesson:** the bullets above; full detail in `.claude/rules/agentic-workflow.md`.
- **Status:** distilled (2026-07-02)
