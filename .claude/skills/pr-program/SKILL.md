---
name: pr-program
description: >-
  Run a MULTI-PR program (audit remediation, God-file split, a batch refactor, a fix-all-findings
  sweep) on Murmur without freezing the Mac. The RAM-safe SEQUENTIAL, CI-gated cadence + the
  pre-push checklist + worktree hygiene + verifier-recovery discipline — proven driving a 12-PR
  program clean. Use whenever the work is "many PRs, one theme" and you'd otherwise be tempted to
  fan out parallel builders (which freezes this 64GB Mac). NOT for a single feature (that's
  /ship-feature); this is the orchestration LAYER that runs many ship-feature-shaped PRs in series.
---

# /pr-program — run a many-PR program sequentially, CI-gated, without freezing the Mac

This is the **orchestration playbook** for a program of PRs (10, 20, 40 of them). `/ship-feature` is
one PR's build→verify loop; this skill is the loop AROUND it: how to sequence many, keep the machine
alive, and let CI be the real gate. It encodes the hard-won discipline from the brain-v3 audit-fix
program (12 PRs, 2 real bugs caught by review, zero shipped regressions) and the day the Mac froze.

## THE RAM LAW (the thing that actually bites)

Murmur's app test binary statically links the ML tree (candle/whisper/onnx); ONE `cargo test --lib`
link is a ~15-20 GB transient. **Several at once — multiple builder/verifier agents each running
their own cargo on the shared `CARGO_TARGET_DIR`, plus your own runs — pin the macOS memory
compressor and freeze the whole UI** (swap stays 0; it's compression, not pageout). After
`pkill cargo rustc` the machine is instantly ~95% free — proving it's transient build peaks, not a
leak. Root cause + fix: `docs/research/2026-07-18-build-ram-freeze.md`.

**Rules, non-negotiable:**
1. **Exactly ONE `cargo` process machine-wide, ever.** One builder OR one verifier OR one of your
   runs — never two. Verifiers do their RED-reverts as SINGLE targeted tests, or run read-only
   (lock-security is mostly static). Never dispatch two cargo-running agents at once.
2. **Never run the full 1900-test suite locally.** Iterate + verify with TARGETED filters
   (`cargo test --lib links` = full compile, catches build errors, runs only that module). **CI (the
   GitHub macOS runner) runs the full `ci.sh` gate — remotely, at 0 local RAM.** Push → let CI gate.
3. **`CARGO_BUILD_JOBS=2 … -j2`.** And ensure `[profile.dev.package."*"] debug = false` is in the
   workspace `Cargo.toml` (it slashes dependency-debuginfo link RAM — land it as PR-0 of any program
   if missing).
4. **Read-only fan-out is the ONLY safe parallelism.** A MAPPING phase (agents reading files to
   categorize by domain, no cargo) can be a Workflow. The EXECUTION (move code + compile) is serial.
   Do NOT reach for a parallel-cargo Workflow.

## THE CADENCE (per PR)

```
worktree off fresh origin/murmur (light profile present)
  → builder (ONE local cargo): writes code, iterates on TARGETED tests,
    runs `cargo clippy --lib -- -D warnings` (link-free CI-parity), greps the diff for MSRV items
  → verify: adversarial-verifier (+ lock-security-reviewer if the change touches seal/gate/visibility)
    — dispatched so only ONE runs cargo (lock-security static/no-cargo alongside adversarial's targeted RED-reverts)
  → apply any FAIL findings, re-prove RED via Edit (NEVER git-checkout an uncommitted tree), narrow re-review of the delta
  → QueaT commit (no Claude trailers; no backticks / no "clippy --all-targets" literal in the message — block-bash refuses it)
  → rebase onto fresh origin/murmur (merge-skew), re-grep MSRV
  → push → PR to murmur → CI (remote, full ci.sh)
  → overlap: start the NEXT builder while this PR's CI runs (still ONE local cargo)
  → CI green → squash-merge → remove the worktree → next
```

Order PRs by dependency (shared-file PRs serialize; disjoint ones can overlap build↔CI). Base each PR
on the LATEST trunk; if two touch the same file, the second rebases + resolves (usually a clean
union — e.g. two additive blocks in `detail.component.ts`).

## THE PRE-PUSH CHECKLIST (each catches a real CI-cycle-waster)

- `cargo test --lib <targeted>` green (NOT the full suite).
- **`cargo clippy --lib -- -D warnings` clean** — link-free, ~15s. Catches the `dead_code` class that
  `cargo test` AND `clippy --lib --tests` MASK (a const/fn used only in `#[cfg(test)]` is "unused" in
  CI's lib-only build; often the feature is inert too). Ate a CI cycle when skipped.
- **MSRV grep**: `git diff origin/murmur -- src-tauri | grep '^+' | grep -oE 'is_none_or|LazyLock|split_at_checked|take_if'`
  → empty. `-D warnings` implies `-D clippy::incompatible_msrv` (MSRV 1.77); `is_none_or`(1.82) → `map_or(true, …)`.
- FE PRs: `npx ng lint && npx ng build` (never `ng test`).

## VERIFIER DISCIPLINE (this is where the real bugs hide)

- **A green `cargo test` proves neither no-deadlock nor no-leak when no test hits the path.** Two real
  bugs shipped green this program: an Accept SELF-DEADLOCK (guard held across a callee that re-takes
  it — no test hit a SUCCESSFUL accept) and a seal-time title LEAK (no test hit the marker's `url`
  field shape nor the canonicalized src-direction). Hunt untested happy-paths + every data SHAPE.
- **RED-reverts use the Edit tool or `git stash`, NEVER `git checkout <file>` on an uncommitted PR**
  (it reverts the WHOLE PR to trunk — a verifier nuked a 1300-line uncommitted PR this way). Restore
  byte-identical + prove `git diff --stat` matches before reporting.
- **Prove a RED test is actually RED** — neuter the fix hunk, confirm the test FAILS, restore. A
  mislabeled regression (row inserted the wrong direction) passes on the old code and pins nothing.
- Each verifier writes its verdict JSON to `.claude/tmp/<task>/`. Under session churn (model/effort
  switches), agents die silently — trust the FILESYSTEM (git status/diff, the verdict JSONs, a dead
  verifier's scratch probe tests you can run yourself), not chat. Respawn fresh; don't nudge a wedged
  agent forever. SHUT DOWN each agent the moment you've consumed its verdict (idle-spam re-fires you).

## WORKTREE HYGIENE (the user's RAM is finite)

- One worktree per in-flight PR; **remove it the instant the PR merges** (`git worktree remove
  --force` skips the slow untracked scan; then `git worktree prune`).
- Reuse a freed worktree for the next PR (`git checkout --detach origin/murmur && git checkout -b …`)
  — node_modules stays warm.
- Periodically prune the stale `.claude/worktrees/agent-*` / `wf_*` auto-worktrees from past
  sessions (they inflate `git worktree` + eat disk). Before removing a NAMED sibling, check it has no
  unmerged committed work another session needs (`git -C <wt> log origin/murmur..HEAD`).

## IDENTITY / GATES (inherited, non-negotiable)

Commits authored ONLY by `QueaT <kgm004a@gmail.com>`, no Claude trailers; `gh` = `JakubGawr`; never
direct-push to `murmur` (PR only); `com.meetnotes.app` immutable. The full `scripts/ci.sh` (clippy
`-D warnings` + tests + `ng lint` + `ng build` + headless E2E) is the release-parity gate — run by CI
on every PR, so you don't run it locally.
