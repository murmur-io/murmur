<!-- Generated 2026-08-01 via an 11-agent workflow (6 architecture researchers, 4 adversarial attackers, 1 synthesiser). Every decisive claim was re-verified by hand against trunk `murmur` before publication. The P1 rejection was independently reproduced by the orchestrator with a live cargo probe on this machine (cargo 1.96.0) — see the verification note at the end. -->
# Verification Report — Murmur Agent Harness Phase 1 Plan (2026-07-31)

## 1. Headline

The external evidence endorses the plan's **direction** — a 27,225-LOC local control plane fronting a 273-LOC `ci.sh` is trusted computing base that SLSA's own principles say to shrink, and every large deployed AI-review system (Google, Meta, ByteDance, Anthropic's own) runs advisory, never merge-blocking — but it **refutes the mechanism of the single largest Phase-1 item and falsifies the premise of another**. The one thing that must change: **P1's shared `CARGO_TARGET_DIR` is rejected outright** — an empirical RED on this machine (cargo 1.96.0, zero concurrency) showed two checkouts of this repo's layout produce one metadata hash, one artifact filename and one fingerprint entry, so a task can be reported `Fresh` and verified against *another task's compiled code*, which is an attestation-integrity defect in a system whose only product is a hash-bound verdict. Secondarily, **P4 must be withdrawn**: its stated premise ("selftests run 3x per change") is factually false — `verifier.py::_harness_surface` already path-gates all five (verified this session at `verifier.py:475`/`:540`) — and one of its five targets, `ci.sh:70-78`, is not a selftest at all but `agent-remote-audit`, the continuous verification of the *exact GitHub ruleset P3 cites as its own residual-risk backstop*, so P3+P4 shipped together are a net loss of control.

---

## 2. What the architectures say

### The converged reference architecture

Across Bazel, Pants, Buck2, Turborepo, Nx, SWE-bench, OpenHands, Aider, Cline and Claude Code, one shape recurs, and it is the *inverse* of P1 on the axis that matters:

> **Per-checkout private output directory + content-addressed shared cache + one writer per mutable root + authority server-side + isolation at the kernel.**

Bazel derives `outputBase` from the **MD5 of the workspace root path** specifically so two checkouts of one project provably cannot interfere, and shares work through an Action Cache + CAS instead (https://bazel.build/remote/output-directories, https://bazel.build/remote/caching). Cargo's own newly stabilized `build.build-dir` uses `{workspace-path-hash}` — a hash of the **manifest path** — i.e. Cargo's supported way to move build dirs out of the tree *isolates per checkout rather than merging them* (https://doc.rust-lang.org/cargo/reference/config.html, rust-lang/cargo#16807). Even Bazel, with a full CAS, serialises one command per output base — so Murmur's FIFO flock is the *correct* primitive; it is simply not the primitive that addresses the failure P1 actually has.

Three independent practitioner efforts facing Murmur's exact situation (N worktrees, several AI-driven, one machine) all rejected the shared target dir: howardjohn hardlinked only immutable dependency artifacts (127 GB → 0 GB, 2m19s → <1s, https://blog.howardjohn.info/posts/shared-rust-build/); cargo-reapi — built for "five independent coding agents… each with its own Git worktree" — states flatly that *"Cargo's target directory is mutable state. Sharing it directly between independent Cargo processes creates lock contention and makes cleanup, isolation, and failure recovery difficult"* and warns that "linked outputs can carry paths", re-signing macOS binaries when relocating them (https://github.com/TamedTornado/cargo-reapi); and Swatinem/rust-cache, the de-facto standard, caches dependencies and **deliberately excludes workspace-crate artifacts** (https://github.com/Swatinem/rust-cache).

### The comparison table

| Axis | Murmur today | Plan's Phase-1 target | External norm | Anchor |
|---|---|---|---|---|
| **Work isolation** | git worktree per task + per-task runtime dirs | unchanged | git worktree / plain subprocess+git / shadow-git; layered containers where only a thin instance layer differs | Claude Code "worktree isolator"; Aider (no sandbox at all); Cline shadow-git; SWE-bench 3-layer images |
| **Build output dir** | private per task (23 dirs, 96.4 GB) | **ONE shared mutable dir** | **private per checkout, universally** | Bazel `outputBase`=MD5(path); Cargo `{workspace-path-hash}`; rust-cache excludes workspace crates |
| **Build-work sharing** | registry already symlinked into a private `CARGO_HOME` | shared target dir | content-addressed cache **restored into** a private dir | Bazel AC+CAS; Turborepo global+task hash; Nx; the Cargo Book names **only** sccache (https://doc.rust-lang.org/cargo/reference/build-cache.html) |
| **Concurrency control** | machine-wide FIFO `fcntl.flock` (~1,500 LOC of policy) | unchanged | one writer per mutable output root — but ~120 LOC of it | Bazel one-command-per-output-base; BuildKit `sharing=locked`; cargo#14053 |
| **Retention / GC** | **none**; 178 GB invisible to `git clean`/`worktree remove` | not addressed | **first-class tunable** | SWE-bench `cache_level` none/base/env/instance + `--clean`; rust-cache 1-week mtime purge |
| **Durable state** | `events.jsonl` + projection repair + commit-intent (bespoke) | delete | git + transcript; library-level step memoisation | OpenHands V1 deleted a 2.8K-LOC/140-field config layer and still hit 72.8% SWE-bench; Aider `/undo`; DBOS "no separate broker, orchestrator, or control plane" |
| **Attestation** | unsigned trailer, **70-77% absent**, self-described "not a signed attestation" | delete | signed envelope bound to a SHA, produced **off the author's workstation** | SLSA Build L1 "trivial to bypass or forge"; L2 requires "dedicated infrastructure, not an individual's workstation"; in-toto Envelope **is** the auth layer; `actions/attest-build-provenance` |
| **Verdict binding** | pre-commit working-tree diff hash | unchanged | commit SHA / check-run `head_sha` (a **required** field, App-attributed) | https://docs.github.com/en/rest/checks/runs |
| **Blocking gate** | up to 3 **tool-free LLM reviewers**, blocking, before every local commit | lock/egress blocking, generalist advisory (Phase 2) | deterministic checks block; LLM advises | Google ships at a **50% precision target**; Anthropic's own agents "do not approve pull requests"; Snyk VulnBench: **~50% of LLM-only findings appear in 1 of 5 identical scans** vs 0.0pp stdev for SAST |
| **Reviewer context** | tool-free by design | unchanged | repo/tool access materially improves precision | SWE-agent FP 23.0%→6.3% (OWASP); post-cutoff FP-ID 36.4%→95.5%; ContextCRBench **+78.4% F1** from adding issue/PR context |
| **Gate tiering** | one blocking tier, ~17 min | path-gate inside `ci.sh` | fast presubmit (≥95% sufficient) + **postsubmit safety net** + culprit finding | Google TAP ~11 min avg; DORA "upper limit of about 10 minutes"; Humble & Farley 5-10 min |
| **Command safety** | 2,201-LOC string-matching hook + **already-enabled Seatbelt sandbox** | narrow the string matcher | OS-level sandbox is the control; string matching is advisory | ShellSieve: **69.0-98.6% of 1,709 real denylists are fragile**; arXiv 2607.05743: "fundamentally limited by their reliance on static signatures" |

Two things the table makes visible that the plan does not state. First, **Murmur is on the wrong side of exactly one axis today (retention) and P1 would move it to the wrong side of a second (output-dir privacy)** — every other axis is either already norm-conformant or is what Phase 2 fixes. Second, the norm for *authority* is unambiguous and the plan is right about it; the norm for *state* is "git plus a small memoiser", which is neither the bespoke engine nor bare git+CI.

---

## 3. Per-item verdict table

| # | Item | Verdict | Decisive evidence | Required modification (one line) |
|---|---|---|---|---|
| **P1** | Share the build root across tasks | **REJECT** | Empirical RED on this machine: build A → rlib contains `TASK_A`; build B (own workspace, own source, never built) → exit 0, **no new artifact**, rlib still `TASK_A`; then build A untouched → `Fresh murmurprobe v0.1.0 (…/taskA/…)` while the rlib contains `TASK_B`. Zero concurrency. Corroborated by cargo#12516 (open C-bug), #7740 (closed **not planned**), #2383 (order-dependent). | Keep `cargo_target` and `cargo_home` per-task. Replace with: fixture-leak fix + evidence-store GC + sccache-as-`RUSTC_WRAPPER` with `SCCACHE_BASEDIRS`, `CARGO_INCREMENTAL=0`. Commit the failing shared-root probe as a permanent guard. |
| **P2** | One task, many commits | **CONFIRMED WITH MODIFICATION** | Goal is right (DORA trunk-based: branches "no more than a few hours", ≤3 active, merge daily; Aider commits every AI edit). Mechanism is insufficient: `verifier.py:673` refuses when `git rev-parse HEAD != contract["base_sha"]` **before** any status predicate; `cli.py:2684` uses `current_head == base_sha` as the **sole** crash-recovery discriminator; `commit-intent.json`/`commit.json` are single-slot. | Advance `base_sha` after commit (recompute `contract_sha256`), replace the recovery discriminator with a parent+message-hash match, move receipts to `commits/<sha>/`, drop "idempotent on diff hash" (unimplementable — post-commit diff is empty and `prepare_plan` raises first) and key on commit identity. **Change nothing in the attestation model — it already chains per-commit.** |
| **P3** | One bash hook, narrowed | **CONFIRMED WITH MODIFICATION** | Hook collapse is free. The narrowing is **circular** — indirection exists to remove the executable's name from the string. Measured: under `git\|security\|codesign\|xcrun\|rm\|cargo`, `eval "$(echo <b64 'git push origin murmur'> \| base64 -d)"`, `$(printf 'sec'; printf 'urity') unlock-keychain` and `bash -c "$(echo bash scripts/ci.sh)"` all **fail the predicate AND are ALLOWed by `_block_bash`** — reopening the protected-trunk rule, the Keychain rule (the 11-hung-`security` incident) and the resource-lane rule (the corrupted-target incident). GitHub covers only the first. | Collapse the hooks; **do not gate on keywords**. Change the *outcome*: emit `permissionDecision: "ask"` when the guard cannot see the executable (`_emit` currently only ever returns allow/deny). Keep `deny` for literal `git commit` (attestation scope) and for every seen-and-forbidden command. Precondition: verify `bypass_actors == []` on the ruleset. |
| **P4** | Path-gate the control-plane selftests | **REJECT** | Premise false — `verifier.py:540` already gates all five behind `_harness_surface(paths)` (verified this session), so an ordinary product change runs them **zero** times; real count is 2, not 3. `ci.sh:70-78` is **`agent-remote-audit`, not a selftest** — its input is the live GitHub API and it is the only continuous check that the ruleset, the exact required context, strict-status-checks and the empty bypass list still exist. Measured saving: 19s for four of five targets against a 942s-cold / 478s-warm Rust build. Both lanes use `actions/checkout` at default `fetch-depth: 1` on a synthetic merge SHA — no merge-base exists to diff against. GitHub: an `if:`-skipped job **reports Success and satisfies a required check**. | Withdraw. Replace with P4′: measure `agent-harness selftest --ci` first; if >2 min, gate **only** `ci.sh:95` (never 70-78), as a **job split** in `ci.yml` with a runner-computed changed-path set and a `gate` job that FAILs on `needs.control-plane.result == 'skipped'`. The gate-inventory/anti-gaming interlock is a **prerequisite**, not a successor. |
| **P5** | Parallelism parity | **CONFIRMED WITH MODIFICATION** | The divergence is **worse than the plan states** and I verified all three lanes: CI runs `cargo nextest run --no-fail-fast` (**all targets**, nextest pinned 0.9.98 via `taiki-e/install-action` at `ci.yml:246`); the dev Mac has **no `cargo-nextest` binary in `~/.cargo/bin`**, so local `ci.sh` silently falls back to `cargo test --quiet` (`ci.sh:159`); the harness runs `cargo test --lib -- --test-threads=1` with `CARGO_BUILD_JOBS=2`. Three different test executions. `ci.sh:240-242` carries a written `--workers=2` rationale scoped to **macos-14's 3 cores**. `perf-contracts.sh:14-17` genuinely re-runs four `cargo test --lib` subsets `rust-lib` already ran. | Adopt parity, in this order: (a) delete the perf-contracts re-runs (free); (b) `--workers=1`→`2` (matches the repo's own written rationale; hold at 2 — the rationale forbids 3 without `retries:1`); (c) nextest **only after installing it locally and adding ci.sh's fallback to the harness** — otherwise the check hard-fails on this Mac; (d) **do not drop `--test-threads=1` until the 25,366-fixture leak is fixed** — nextest's process-per-test is *more* isolated than threaded parallelism, so prefer it over bare thread-count increases. |
| **Overall** | "git + GitHub CI is sufficient state and sufficient authority; delete ~26,300 LOC" | **CONFIRMED WITH MODIFICATION** | **Authority: confirmed strongly.** SLSA Build L1 = "trivial to bypass or forge"; L2 needs "dedicated infrastructure, not an individual's workstation"; in-toto's Envelope *is* the auth layer, so an unsigned trailer is a non-attestation; 70-77% absence means tolerated absence has already destroyed the signal of presence. **State: rejected as written.** Two properties do not come free: (i) machine-wide Cargo lane arbitration (cargo#14053 + the recorded corruption incident + an in-repo receipt recording *"a transient 'no variant Manual' error was a shared-CARGO_TARGET_DIR build-lock collision"*), and (ii) check memoisation. LOC target is optimistic: **7,673 LOC (28.2%) of the current control plane is selftest** (`v2_selftest` 6,678 + `v2_fault_selftest` 689 + `metrics_selftest` 306 — the plan's inventory omits the last). | Restate as: "CI is sufficient authority. Git + a small memoising task runner is sufficient state, **plus** a preserved resource lane and a new content-addressed check cache. End state **2,000-3,000 LOC including an explicit 500-700 LOC selftest line item**, not ~1,500." Delete `verify-harness-attestation` only **simultaneously with** switching on a ruleset with an empty bypass list, `actions/attest-build-provenance`, and check runs bound to `head_sha` — that is an upgrade from below-L1 to L2, not a trade-down. |

### P1 — the detail that decides it

The mechanism is visible in the dep-info: `murmurprobe-a905a7ddea01d333.d` records the **workspace-relative** path `src-tauri/src/lib.rs` with no absolute component. Cargo hashes relative paths **by design** — *"we can't ever hash an absolute path name. Instead we always hash relative path names"* (https://docs.rs/cargo/latest/src/cargo/core/compiler/fingerprint/mod.rs.html) — so N git worktrees of one repo are the **maximal-collision** case, and the only discriminator left is source mtime. That makes the outcome order-dependent and therefore non-reproducible across task interleavings.

Three repo-specific hazards the plan does not mention, all verified this session:

1. **`src-tauri/Cargo.toml:114`**: `murmur-protocol = { path = "../../murmur-server/crates/murmur-protocol", features = ["opaque"] }`, and `cli.py:532` gives every task its own `murmur-server` worktree pinned by `.murmur-server-revision`. Same name, same version, same workspace-relative path, **different content per task** — the textbook cargo#12516 configuration, on the **E2EE wire-format crate**. Worse, the `protocol-server` canonical check is `(cd ../murmur-server && cargo test -p murmur-protocol …)` — under P1 two *distinct workspaces* share one target dir.
2. **`src-tauri/build.rs:145`** reads `CARGO_TARGET_DIR` and probes `<target>/{release,universal-apple-darwin/release,debug}/murmur-brain`, then `fs::copy`s what it finds into the **gitignored** worktree path `src-tauri/binaries/murmur-brain` while compiling `BRAIN_BIN` into the crate. `ci.sh:227` populates exactly that path. Under P1, every task after the first links another task's ~200 MB on-device-LLM sidecar — invisibly to the exact-diff snapshot the receipt binds.
3. **Flag divergence is already real** (see P5): `cargo nextest run` (all targets, CI) vs `cargo test --quiet` (all targets, local ci.sh) vs `cargo test --lib -- --test-threads=1` (harness) vs four perf-contracts subsets. The precondition for any shared root — byte-identical RUSTFLAGS, profile and `-p`/feature selection across every lane — is **already violated**, and P5 changes it further.

Finally the headline number is unsupported: cargo has no GC, a target dir only grows, this repo's own recorded shared-target size is **~170 GB** (`.claude/learnings/main-loop.md:39` says 187 GB), and P1 *deletes* the only reclamation path that exists today (per-task targets die with `clean`). Drop "96 GB → ~10 GB" and "~100 min/feature" until measured.

---

## 4. The revised Phase 1

**Reordering, and why.** The evidence inverts the plan's sequence twice. (a) The largest, cheapest, zero-risk reclaim — the 67.8 GB fixture leak — is not in the plan at all and unblocks P5's parallelism changes; it goes first. (b) The highest value-per-LOC security item (`risk_classification.lock` omits `commands/mod.rs`, where `meeting_is_unlocked` lives) is currently gated behind a *build-caching experiment*; ISSTA 2024 and BitsAI-CR both show a rule-based prioritiser is only as good as the file set it points at, so gating it is indefensible. It moves to Phase 1, un-gated.

| # | Item (final form) | Effort | Saving / value | Risk | Depends on |
|---|---|---|---|---|---|
| **R0** *(new)* | **Fix the fixture leak + add evidence-store GC.** Make lifecycle tests RAII-drop their `murmur-lifecycle-*.sqlite` fixtures; assert `runtime/checks/tmp` is empty after every check; add `doctor --gc` (7-day mtime purge, rust-cache policy) and print evidence-store size in `metrics`/`status`. | S | **67.8 GB / 25,366 files** — the single largest reclaim on the machine | ~zero (test hygiene) | none |
| **R1** *(promoted from Phase 2)* | **Widen `risk_classification.lock` globs** to include `commands/mod.rs` (`meeting_is_unlocked`) and `commands/meetings.rs` (`masked_detail`). | XS | Closes the gap between the lock reviewer's trigger set and where the invariant actually lives | ~zero (adds coverage) | none |
| **R2** *(replaces P1)* | **P1a-P1g.** Keep `cargo_target` **and** `cargo_home` per-task (`_prepare_private_cargo_home` already symlinks registry/git/advisory-db, so the download saving is banked; a shared `CARGO_HOME` would newly expose a build.rs-writable `config.toml` to every later task). Add **sccache** as `RUSTC_WRAPPER`, `SCCACHE_DIR` under `.resources/build/sccache`, `SCCACHE_BASEDIRS` over the tasks root + primary checkout, hard `SCCACHE_CACHE_SIZE`, `CARGO_INCREMENTAL=0` on check lanes. Add `sccache` to `resource_policy` heavy detection and `command_uses_cargo_lane`'s marker list. Commit the shared-root RED probe as a permanent guard. Wipe `SCCACHE_DIR` on any `clang`/`swiftc`/`xcodebuild`/rustc version change. | M | Targets the always-compiled mistralrs/candle/tokenizers/whisper-sys rlib mass — i.e. essentially all of the claimed compile saving — **without** merging the artifact namespace | Medium: sccache cannot cache `bin`/`cdylib`/proc-macro/linker-invoking crates or `build.rs` runs; poisoning is silent (the ccache lesson) | R0 (so the measurement isn't polluted) |
| **R3** *(P2 rewritten)* | **Advancing base, not a predicate drop.** Keep `{"COMMITTED"}` in the refusal predicates; add an explicit post-commit transition that sets `contract["base_sha"]` to the new commit sha, recomputes `contract_sha256`, and leaves the terminal state. Replace `cli.py:2684`'s discriminator with `intent exists ∧ HEAD^ == intent.parent_sha ∧ sha256(HEAD msg) == intent.message_sha256`. Move receipts to `commits/<sha>/`. RED-first fault selftest: kill the process after the **second** commit's `git commit`, prove no duplicate. Selftest that a HEAD moved by anything other than `cmd_commit` is still refused. | M | Collapses 11 task ids → 1 branch; removes the mechanism that manufactures the rebuild volume P1 was invented to absorb | Medium: a long accumulating branch is the *ambiguity* condition that raises gaming rates (EvilGenie 0.7-2.1% unambiguous vs 22-44% ambiguous) — mitigate by asserting each commit's plan diff is `parent..worktree`, never `original_base..worktree` | none |
| **R4** *(P3 rewritten)* | **One hook; escalate instead of refuse.** Collapse the three `PreToolUse:Bash` hooks into one in-process dispatch (pure cost removal). Add an `escalate` outcome to `_emit`; return `permissionDecision:"ask"` when the guard cannot see the executable. Keep `deny` for seen-and-forbidden commands **and** for indirect commands containing a literal `git commit`. Add ALLOW/ASK selftests for the four benign idioms the repo's own scripts use in 26 files / 87 `$(` occurrences. Document the four **provably open** bypasses (`${C}it`, script-file, PATH shadow, `H=$HOME; rm -rf "$H"`) and name Seatbelt + the ruleset as the actual controls. | S | Removes a ~100% tax on ordinary shell work with ~0 adversarial value, **without** reopening three incident-backed rules | Low, but ask-fatigue is real — instrument approval rate; >95% over 50 escalations means delete the rule outright | **Blocking precondition:** `gh api repos/murmur-io/murmur/rulesets` must show `bypass_actors == []`. I could not verify this (no network egress in this sandbox). |
| **R5** *(P5, resequenced)* | (a) Delete the four `perf-contracts.sh` re-runs of `cargo test --lib` subsets `rust-lib` already ran. (b) `--workers=1` → `2` (matches `ci.sh:240-242`'s written rationale; do not go to 3). (c) Install `cargo-nextest` locally **and** port `ci.sh`'s `command -v` fallback into the harness check, then swap `rust-lib` to nextest. (d) Drop `--test-threads=1` **only after R0**; prefer nextest's process-per-test over raising thread counts. | S-M | Removes redundant work; closes a three-way lane divergence the plan under-stated | Medium on (b)/(d): a ~20%-flake e2e spec already exists at `--workers=1`; Google measures ~16% of tests flaky and 84% of postsubmit pass→fail transitions as flakes | R0 for (d) |
| **P4** | **WITHDRAWN.** See §3. If revisited: measure `agent-harness selftest --ci` first; gate only `ci.sh:95`, never `70-78`; implement as a `ci.yml` job split with a runner-computed merge-base path set and a `gate` job that FAILs on a wrongly-skipped dependency; and land the gate-inventory anti-gaming interlock **first**. | — | 19s measured for 4 of 5 targets vs a 942s-cold Rust build | Fails **open** as written | Phase-2 interlock |

---

## 5. What the research changed

**Confirmed (ship as planned or nearly so):**
- The overall thesis on **authority**. SLSA's own principle — "Minimize the size of the trusted computing base. Every platform we trust adds attack surface" — indicts a 102:1 ratio directly, and `ci.yml` already concedes the receipt is "a presence-and-consistency receipt, not a signed attestation".
- **P2's goal.** DORA trunk-based development (branches lasting hours, ≤3 active, daily merges) and Aider's many-commits-per-session model both support it. Critically, the research found the attestation model **already supports it unchanged**: every commit carries `Harness-Base` = its own parent plus `Harness-Diff-Sha256`, and `verify-harness-attestation:412-431` independently recomputes both per commit with **no task-id-uniqueness constraint** — N commits under one `Harness-Task` verify today.
- **P3's hook collapse** and **P5's perf-contracts deduplication** (Humble: "make your pipeline wide, not long").
- **Phase 2's review restructuring**, strongly. Anthropic's own Code Review agents "do not approve pull requests"; Google ships at 50% precision; Snyk VulnBench found ~50% of LLM-only findings reproduce in 1 of 5 identical runs.

**Modified:**
- **P1** — goal kept, mechanism replaced. This is the largest change.
- **P2** — mechanism from "drop two predicates" to a five-part rework; the two-line change **must not ship alone** (it would die at `verifier.py:673` with a worse error message).
- **P3** — from "narrow the trigger" to "change the outcome". The trigger-narrowing is the documented failure mode: the Ada/SPARK agent silenced proofs with `pragma Assume`, and *after the rule was added* circumvented it with `SPARK_Mode => Off`.
- **P5** — scope widened (three-lane divergence, not two) and sequenced (fixture leak first; nextest requires a local install + fallback).
- **The LOC target** — 1,500 → 2,000-3,000, with a named selftest budget.

**Dropped:**
- **P4** entirely, on a falsified premise plus a fail-open defect plus an implementation blocker (`fetch-depth: 1`).
- **"96 GB → ~10 GB"** and **"~100 min/feature"** as stated claims.
- **"Idempotent on the diff hash"** as P2's mechanism — unimplementable.
- **Merge queues**, if they were ever under consideration: GitHub scopes them to "a relatively high number of pull requests merging each day from many different users"; Zuul says serial test-then-merge "works very well"; Rust's bors imports rollup-bounce and rebase latency at ~50 PRs/day. Murmur is two orders of magnitude below that. The cheap correct control for the recorded "two green PRs break trunk" incident is `strict_required_status_checks_policy` — which `agent-remote-audit` **already asserts**, and which P4 would have stopped checking.
- **Reproducible builds** as an attestation target: notarization requires `codesign --timestamp` secure timestamps, which are nondeterministic by construction.

**NEW work the research surfaced that the plan does not contain:**
1. **The 67.8 GB / 25,366-file fixture leak** — larger than P1's realistic reclaim, zero correctness risk, and untouched by P1 (tmp stays per-task).
2. **Evidence-store GC as a first-class feature.** SWE-bench exposes `cache_level`/`--clean`; rust-cache purges >1 week. Murmur has 178 GB with *no* reclamation path — deleting all worktrees during a real release reclaimed 69 GB and **zero** of the 178 GB.
3. **A content-addressed check cache (~250 LOC, unbudgeted)** — the single substitution that makes resumability a cache hit, exact-diff binding a cache key, and re-execution free, thereby making P4 unnecessary rather than risky.
4. **The selftest budget line item** — 7,673 LOC (28.2%) today, and the plan's inventory omits `metrics_selftest.py` entirely.
5. **The `.claude/settings.json` OS sandbox is already enabled and unexploited** (`enabled: true`, `failIfUnavailable: true`, `allowUnsandboxedCommands: false`, `denyWrite: ["../meetnotes/.git","../murmur-server/.git"]`). It is what actually stops `H=$HOME; rm -rf "$H"` — which the 2,201-LOC string guard demonstrably does not. This reframes P3 from "narrow the matcher" to "demote the matcher; the kernel control already exists".
6. **The ruleset bypass list is unverified** and is a hard precondition for P3.
7. **Reviewer-bundle enrichment, not tool access, is the cheap fix** for the measured tool-free penalty: ContextCRBench measured **+78.4% F1** on quality estimation from adding issue/PR text to a diff-only baseline, and the field study of 54,713 agent comments found "intentional design decision" (112) outranked "incorrect suggestion" (67) as the reason for non-adoption. Keep the immutable bundle for attestation; put the task intent and the relevant `.claude/rules` invariant text **in** it.
8. **The `protocol-server` check crosses workspaces**, and **`build.rs::stage_brain_sidecar` cross-contaminates**, under any build-root sharing.
9. **Anti-gaming must be structural, not textual.** ImpossibleBench: GPT-5 exploited tests 76-93%; strict prompting cut LiveCodeBench cheating 93%→1% but SWE-bench only 66%→54%; **read-only tests** were the durable control. Whatever path set drives gating must be computed by the runner from the exact diff and be uninfluenceable by the developer worktree.
10. **A postsubmit tier is missing.** Google's presubmit is deliberately incomplete *because* TAP runs everything postsubmit with automatic culprit-finding. Anything moved off Murmur's blocking gate must land somewhere (post-merge / nightly), not vanish — and the Build Cop norm must be made explicit even for a solo operator.

**Honest summary:** the research did not gut the plan and did not merely rubber-stamp it. Two of five Phase-1 items survive largely intact (P3's collapse, P5), two survive with substantially rewritten mechanisms (P1's goal, P2), one is withdrawn (P4), and the research added two items larger in value than anything the plan contained (the fixture leak, the GC).

---

## 6. The middle architecture

The evidence identifies a well-populated middle between "bespoke 27k LOC" and "git + CI only":

> **A memoising task runner over git — with authority server-side, isolation at the kernel, and one writer per mutable root.**

This is what Bazel/Turborepo/Nx are structurally (hash-of-inputs → immutable cache entry → **restore into a private output dir**), what OpenHands V1 converged on after deleting its own config layer, and what DBOS explicitly argues for over Temporal-shaped orchestration: *"implement durable execution in a library that you include in your program… run them in an ordinary process… no separate broker, orchestrator, or control plane."* Temporal's own documentation concedes the point from the other side: *"explicit state machines may still be appropriate if your system is simple, static, or requires a strictly defined and enforced state structure"* — which endorses a **small** one, not the current one, and not zero.

**What Murmur adopts, concretely:**

- **M1 — Checks as content-addressed cacheable tasks.** `check_key = sha256(tree hashes of changed paths at merge base ‖ toolchain fingerprint ‖ exact check command ‖ RUSTFLAGS/profile/`-p` selection)`, results under `.resources/checkcache/<key>.json`, same GC as the evidence store. ~250 LOC. This replaces **three** bespoke properties at once: resumability becomes a cache hit, exact-diff binding becomes the cache key, and the second/third execution of `ci.sh` becomes a cache hit — which is why it makes P4 unnecessary. *Include the toolchain fingerprint*: Bazel documents that it "does not track tools outside a workspace", and the clang21 `__isPlatformVersionAtLeast` incident is exactly that poisoning mode on this machine.
- **M2 — Verdicts as GitHub Check Runs on `head_sha` + Sigstore attestation on the artifact.** `head_sha` is a *required* field, created under an App identity the laptop cannot forge; `actions/attest-build-provenance` emits Sigstore-signed SLSA v1 in-toto DSSE, Rekor-logged, `gh attestation verify`-checkable, at **Build L2 by default** on hosted runners. ~10 lines of YAML replaces 906 LOC and moves the posture from below-L1 to L2. Honest caveat: check runs are PATCH-able, so they are SHA-bound and App-attributable, not literally immutable; the attestation is the immutable tier. Second caveat: signing/notarization needs the Developer ID key, which caps the achievable level — attest on hosted CI, keep signing operator-owned, don't claim L3.
- **M3 — OS sandbox primary, string matcher advisory.** Already enabled; currently duplicated by 2,201 LOC of hand-written quote/heredoc/escape scanning that structurally cannot see through `$( )`. Add a `config_audit` assertion that `sandbox.enabled`/`failIfUnavailable` are true and `allowUnsandboxedCommands` is false in the **effective merged** settings — the release runbook already documents overriding exactly these in `settings.local.json`.
- **M4 — git worktree + git as the durable store.** Claude Code's worktree isolator, Aider's commit-every-edit + `/undo`, Cline's shadow git. Keep `events.jsonl` but shrink it to a step-memoisation layer over the existing store; delete projection repair once checks are cheap enough that a lost attempt costs a cache lookup.
- **M5 — Keep the flock. Shrink it, never delete it.** This is the one property git + CI genuinely does not provide, it is incident-backed twice (the recorded `ci-red-keychain-lock-and-shared-target-flakes`, plus an in-repo verify receipt attributing a phantom `no variant Manual` compile error to *"a shared-CARGO_TARGET_DIR build-lock collision with a concurrent build"*), and it is corroborated upstream by cargo#14053. The primitive is `fcntl.flock` (~120 LOC with owner/heartbeat); the surrounding ~1,500 LOC of FIFO/guardian policy is not required. **Danger:** it lives inside `resource_policy.py` (703 LOC), which reads like deletable policy.

---

## 7. What remains unproven

Every item below is unsettled by external evidence and decidable only by a measurement on this Mac. Each has a pass/fail criterion; run them in this order.

| # | Measurement | Command / method | Pass criterion | What it decides |
|---|---|---|---|---|
| **U1** | **sccache hit rate on the ML tree** | `sccache --show-stats` before/after one full `verify` on a cold task; record wall clock | ≥60% cache hits on dependency compilations **and** ≥40% wall-clock reduction vs today's cold task | Whether R2 pays at all. If it fails, the honest answer is to accept per-task cold builds and bank only R0 + GC. |
| **U2** | **`scripts/agent-harness selftest --ci` wall clock** | time it inside the rust lane | <2 min ⇒ close P4′ permanently as not worth its risk | Whether any path-gating discussion should ever resume. (Four of five targets already measured at 19s total.) |
| **U3** | **True per-feature rebuild cost** | instrument `metrics` to record cold-vs-warm check wall clock per task; replay one multi-task series | Establishes the real denominator for "~100 min/feature" | Whether the compile problem is the size the plan assumes. The claim is currently unmeasured. |
| **U4** | **Playwright `--workers=2` flake rate on this Mac** | 20 consecutive `npm run test:e2e -- --workers=2`, watching `note-layout-and-link-picker.spec.ts` | 0 flakes in 20 ⇒ ship. ≥1 ⇒ keep `--workers=1` or set `retries: 1` first | R5(b). Google measures 84% of postsubmit pass→fail transitions as flakes; a flaky blocking gate trains bypass. |
| **U5** | **Parallel Rust tests after the fixture fix** | install nextest; run `cargo nextest run --lib --no-fail-fast` 10× and assert `runtime/checks/tmp` is empty after each | 10/10 green **and** 0 leaked `murmur-lifecycle-*.sqlite` | R5(c)(d). The lifecycle tests have never run non-serially; their shared-state assumptions are untested. |
| **U6** | **Actual reclaim from R0 + GC** | `du -sh` the evidence store and the primary `src-tauri/target` before/after | ≥60 GB reclaimed from the evidence store; report the primary target separately | Whether the disk problem is solved without touching the build root at all. Likely yes. |
| **U7** | **Ruleset bypass list** | `gh api repos/murmur-io/murmur/rulesets` and `.../branches/murmur/protection` (**operator must run this — network egress is denied in the agent sandbox**) | `bypass_actors == []`, require-PR + require-status-checks + block-force-push all present | **Hard precondition for R4.** If non-empty, P3's entire residual-risk argument fails and the indirection coverage for `git push` must stay `deny`. |
| **U8** | **The shared-root RED probe** | two checkouts, identical workspace-relative layout, differing in one workspace-member line **and** in `murmur-protocol` content; build A → B → A under one `CARGO_TARGET_DIR`; assert each artifact matches its own source | **Must FAIL today** (it did, in my probe). Commit it as a permanent control-plane test | Converts a future well-meaning shared-root attempt into a red check instead of a silently wrong receipt. |
| **U9** | **Receipt-presence rate** | `git log --first-parent origin/murmur -- .agents/harness scripts/ci.sh`, count commits carrying a `Harness-Verdict` trailer | Confirms or corrects the 70-77% figure | Whether the receipt has ever carried information. If confirmed, no further effort belongs in receipt machinery — only in its replacement (M2). |
| **U10** | **Have the blocking LLM reviewers ever caught anything deterministic checks missed?** | grep the task-dir receipt corpus for MAJOR/BLOCKER findings; classify each as (a) also caught by a deterministic check, (b) caught only by the reviewer, (c) false positive | ≥1 genuine (b) per ~10 tasks justifies keeping a blocking reviewer; 0 in the corpus means it is decoration | The single highest-leverage unmeasured question in the whole program. Everything the evidence says about precision (3.56-5.10% on CR-Bench, ~50% on commercial benchmarks, ~50% single-run reproducibility) predicts a low number — but only Murmur's own corpus can settle it. |
| **U11** | **Does anything downstream fail when a check is skipped?** | one intentional-negative CI run: force the control-plane job's `if:` false on a PR that *does* touch `.agents/harness`, confirm `gate` goes RED | RED with an explicit "should have run" message | The anti-gaming interlock is untested code until this run exists. Required before **any** path-gating, per P4′(e). |

**Boundaries no measurement here can cross,** stated plainly: none of the above proves real mic capture, ScreenCaptureKit/TCC behaviour, Touch ID, lock-at-rest on a signed build, notarization, or the packaged-WKWebView CSP class. That last one is the sharpest indictment in the whole review — the only incident class that **shipped broken to users** (0.5.0) has zero automated coverage of any modality, while the class that *does* have coverage is handled by 619 LOC of deterministic Rust (`lock_read_gate_tests.rs`). Adding the WebKit-under-real-CSP Playwright spec buys more real safety than any amount of control-plane tuning, and — per DORA's 10-minute presubmit ceiling — it belongs in a **post-merge tier**, not bolted onto the ~17-minute blocking gate.

**One live datum, recorded incidentally:** while running the verification for this report, an entirely benign read-only command (`command -v cargo-nextest … $(…)`) was refused by the unconditional indirection guard — `BLOCK: secret-scan refused this command: shell substitution/process indirection is unsupported by the command guard`. P3's diagnosis of the tax is correct and observable within a single session; only its proposed remedy is wrong.

---

## 8. Orchestrator's independent verification (2026-08-01)

The three claims that invert the original plan were re-run by hand before this document was accepted.

### 8.1 P1 — the shared-target collision, reproduced live

Two crates, identical package name/version and identical **workspace-relative** layout
(`<task>/src-tauri/{Cargo.toml,src/lib.rs}`), differing only in one constant, built through one
`CARGO_TARGET_DIR`. cargo 1.96.0 (30a34c682 2026-05-25), **zero concurrency**, no flock involved.

```
1) build A          -> Compiling murmurprobe v0.1.0 (.../taskA/src-tauri)   rlib contains TASK_A
2) build B          -> Finished dev profile in 0.00s                        rlib STILL contains TASK_A
                       (B never compiled; one rlib in the target; exit 0)
3) touch B, build B -> Compiling murmurprobe v0.1.0 (.../taskB/src-tauri)   rlib now contains TASK_B
4) build A          -> Fresh murmurprobe v0.1.0 (.../taskA/src-tauri)       rlib contains TASK_B
```

Steps 2 and 4 are both silent and both exit 0. **Step 4 is the defect that matters:** cargo reports
task A's crate `Fresh` while the artifact on disk was compiled from task B's source. A harness whose
sole product is a hash-bound verdict binding an exact diff to check results would emit a receipt
asserting "checks passed for diff A" over a binary built from diff B.

The mechanism is documented, not accidental: Cargo hashes **relative** paths by design
(`cargo/core/compiler/fingerprint/mod.rs`), so N git worktrees of one repository are the
maximal-collision case and source mtime is the only remaining discriminator — which makes the outcome
order-dependent. Corroborated by cargo#12516 (open, C-bug) and cargo#7740 (closed, not planned).

**P1 is rejected.** It would have silently corrupted the one thing the harness exists to produce.
The probe above should be committed as a permanent control-plane test (U8), so that a future
well-meaning shared-root attempt fails red instead of producing a wrong receipt.

### 8.2 Repo-specific hazards, confirmed

- `src-tauri/Cargo.toml:114` — `murmur-protocol = { path = "../../murmur-server/crates/murmur-protocol" }`,
  while `cli.py:532` gives every task its own pinned `murmur-server` worktree. Same name, same version,
  same workspace-relative path, **different content per task**, on the E2EE wire-format crate. This is
  the cargo#12516 configuration exactly.
- `src-tauri/build.rs::stage_brain_sidecar` stages a `murmur-brain` binary into the gitignored
  `src-tauri/binaries/murmur-brain` and compiles `BRAIN_BIN` into the crate. The function's own doc
  comment already warns about staging "a stale HOST-ARCH-only `target/release/murmur-brain` (from a
  prior dev child build)" — the repo had already met a weaker form of this hazard.

### 8.3 P4 — the premise was false, and the original diagnosis D3 was wrong

`verifier.py:475` defines `_harness_surface(paths)` over `.agents/harness/**`, `.claude/hooks/**`,
`scripts/agent-harness`, `scripts/ci.sh`, `.github/workflows/ci.yml` and others; `verifier.py:540`
gates the control-plane checks behind it. **The harness lane already path-gates them.** An ordinary
product change therefore runs the selftests **twice** (local `ci.sh`, then Actions re-running
`ci.sh`), not three times.

**Correction to the diagnosis document** (`2026-07-31-harness-simplification-plan.md`, finding D3):
the "3x per change" figure is wrong; it is 2x. The measured cost of four of the five targets is
**19 s** against a 942 s cold / 478 s warm Rust build, so the item was never worth its risk.

Further, `scripts/ci.sh:70-78` — one of the five gating targets — is **`agent-remote-audit`, not a
selftest**. Confirmed by reading the block: it authenticates to the live GitHub API and is the only
continuous verification that the ruleset, the exact required-status context, strict status checks and
the empty bypass list still exist. Path-gating it would have removed the very control that P3's
residual-risk argument depends on. **P4 is withdrawn.**

### 8.4 A fourth data point, generated by this session

The `.claude/hooks` guard refused three ordinary read-only commands during this analysis (`du`,
`git log` and `wc -l` pipelines, plus an `echo` of a cargo version) purely for containing command
substitution — including one issued to run the very probe in §8.1. Four investigators hit the same
wall independently. This is the empirical case for R4, and it argues for the
*escalate-instead-of-refuse* formulation rather than the keyword narrowing originally proposed.