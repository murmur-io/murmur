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
  4. **Unguarded IPC effect** — an effect-orchestrated fetch without a stale-result guard
     (NG0600/`allowSignalWrites` is gone since Angular 19 — flag any attempt to reintroduce it).
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
- **NEVER `git checkout <file>` / `git restore` / `reset --hard` on an UNCOMMITTED tree to undo a
  RED-revert.** A PR under review is usually uncommitted working changes; a `git checkout db.rs`
  reverts the ENTIRE PR to trunk, not just your scratch hunk (this happened — a verifier nuked a
  1300-line uncommitted PR and had to reconstruct it from a captured diff). For a RED-revert: use the
  **Edit tool** to change the one hunk and Edit it back, or `git stash push -- <file>` → test →
  `git stash pop`, or copy the file to a scratch path. After ANY revert, prove the tree is restored
  byte-identical (`git diff --stat` matches the builder's, no stray hunks) before reporting.
- **A green `cargo test --lib` proves NEITHER no-deadlock NOR no-leak when no test exercises the
  path.** Two real bugs shipped green this program: (1) `accept_link` self-deadlocked (held a
  non-reentrant guard across a callee that re-takes it) — every accept test hit only REFUSAL paths,
  none a SUCCESSFUL accept; (2) the seal-time title-strip leaked because no test hit the auto-related
  marker shape (title in `url` not `detail`) nor the canonicalized src-direction. HUNT: for a
  state-machine/guard change, is there a test on the SUCCESS/happy path (not just the error paths)?
  For a strip/filter/gate, enumerate every SHAPE the data takes (both edge directions, every field a
  marker can live in, every render/sanitize form) and confirm a test covers each — a test that only
  covers the shape the builder thought of green-washes the others.
- **Prove a RED test is actually RED — don't trust its label.** A "src-leg regression" that inserts
  the row in the OTHER direction (because the writer doesn't canonicalize the way the author assumed)
  passes on the OLD code too and pins nothing. Neuter the exact fix hunk (via Edit), run the test,
  confirm it FAILS, restore — a RED test that stays green against the unpatched code captured no bug.

## Run journal
<!-- Append-only, newest first. -->

### [2026-07-17 link-picker pagination] Playwright `reuseExistingServer` on the fixed :4210 silently tested ANOTHER checkout's build
- **Pattern:** mid-verification e2e runs flipped green→red for no code reason — `playwright.config.ts`
  has `reuseExistingServer: true` on the fixed port 4210, and a CONCURRENT session's `ng serve`
  (cwd = the main checkout, pre-fix bundle) was holding the port, so the specs silently exercised the
  WRONG tree. Evidence gathered while any other checkout can own :4210 is unreliable in both
  directions (false RED here; a false PASS is equally possible).
- **Caught by:** adversarial-verifier (probe results contradicted the code under review; `lsof -p` of
  the :4210 owner showed a foreign cwd).
- **Lesson:** before trusting any e2e evidence, prove WHICH tree the server on the port is serving
  (`lsof -ti :4210` → `lsof -p <pid> | grep cwd`). When more than one checkout/session is alive, run
  the suite on a private port with `reuseExistingServer: false` (temp config), never the shared 4210.
- **Status:** journal

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

### [2026-07-17 claude-code-hermetic-mcp] Hunt DENYLIST-FAILS-OPEN in any "sandbox"/isolation control; LIVE-probe CLI egress fixes
- **Pattern:** a "hermetic" subprocess LLM sandbox was built as a DENYLIST (`--disallowedTools <known built-ins>`) — so MCP tools (`mcp__*`) and any future built-in tool were IMPLICITLY ALLOWED, letting a "nothing-leaves" `claude_code` run reach the user's ambient MCP servers (Gmail/Drive/self-referential murmur) with meeting content, bypassing consent + redaction + ledger. Every unit test was green; the hole was in what the DENYLIST didn't name.
- **Lesson:** (1) whenever a security control blocks-known-bad (a denylist of tools/extensions/paths/domains/env-vars), ask "what does a NEW or DYNAMIC item do?" — if it fails OPEN, that's the bug; the correct design is allow-known-good. Grep for `--disallowedTools`, deny-lists, `_ => allow`, block-lists in egress/tool/consent paths. (2) For a CLI-spawn egress fix, static arg-assertions are necessary but NOT sufficient — LIVE-PROBE the real installed CLI to prove (a) the isolation flags don't ERROR the run (an empty `--allowedTools` or a bad flag would break every note/Ask — WORSE than the bug) and (b) the OLD shape actually leaked while the NEW shape doesn't (I ran `printf 'ping' | claude -p --system-prompt … --allowedTools '' --strict-mcp-config` → clean exit 0; the old `--disallowedTools` shape bled ambient MCP context). (3) Prove a conjunction test binds each leg independently (RED-mutate ONLY the strict-mcp flag, then ONLY the allowlist) so a green isn't vacuous. (4) For a "route everything through one seam" fix, grep ALL spawn sites of the binary to confirm none bypass the seam.
- **Status:** journal
