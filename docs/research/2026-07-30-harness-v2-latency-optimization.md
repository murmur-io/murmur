<!-- Generated 2026-07-30 via /research (murmur-researcher fan-out). Tool versions and public product behavior are point-in-time. -->
# Research: Harness v2 latency optimization

## TL;DR / Verdict

Yes. Murmur can likely cut the wall time of a successful mixed Rust + Angular
verification by roughly **35–50%**, and cut many failed repair rounds much more,
without weakening the final exact-diff receipt.

The current trust model is not the problem. The expensive ordering is:

```text
immutable snapshot
  -> every deterministic check, sequentially
  -> fresh reviews
  -> only now discover a source/design defect
```

The recommended shape is:

```text
immutable snapshot
  -> cheap structural preflight + source-only Codex reviews
  -> stop early on a source/design defect
  -> if READY_FOR_EVIDENCE:
       Rust/Cargo lane --------\
                                -> fresh evidence-bound Codex reviews -> receipt
       Angular/Playwright lane -/
```

The source-only review is advisory and can never mint `PASS`. The final review,
specialist review, receipt, commit binding, and full GitHub CI remain unchanged.
Do not put an implementation model or automatic repair loop back inside the
Harness.

The first five changes should be:

1. Run an advisory source/contract review before heavyweight checks.
2. Execute independent Rust and Web check chains concurrently while retaining
   one serialized Cargo lane.
3. Change Harness Playwright from `--workers=1` to the already-proven CI value
   `--workers=2`.
4. Provision the same pinned `cargo-nextest` used by CI and benchmark it as the
   Harness Rust test runner.
5. Stop `perf-contracts` from rerunning Rust tests already covered by
   `rust-lib`.

Do not migrate Murmur to Bazel, Pants, Nx, Temporal, or LangGraph. Borrow their
action hashing, affected-selection, and durable retry semantics; the operational
systems themselves are unnecessary for a single-repository, single-Mac verifier.

## What we already have

Harness v2 already has the difficult trust properties:

- It is verifier-only; implementation and repair stay outside the trust plane.
  [`.agents/harness/README.md`](../../.agents/harness/README.md)
- Changed paths and explicit claims derive canonical checks. Any Rust source
  selects the full `rust-lib` command, Angular changes select lint/build, and
  behavioral UI changes select the entire Playwright suite.
  [`.agents/harness/verifier.py`](../../.agents/harness/verifier.py#L496)
- Each attempt is bound to the exact diff, tree, plan, protocol, commands,
  sandbox environment, logs, and review prompts. Green checkpoints survive only
  a resume of the same attempt.
  [`.agents/harness/cli.py`](../../.agents/harness/cli.py#L1671)
- Reviews already run concurrently, with up to three fresh tool-free sessions.
  [`.agents/harness/cli.py`](../../.agents/harness/cli.py#L1972)
- Cargo-heavy work uses one machine-wide FIFO resource lane. Check duration
  currently includes both queue wait and the entire compile + test/runtime
  process.
  [`.agents/harness/runtime.py`](../../.agents/harness/runtime.py#L2090)
- CI already separates Rust and Web into parallel jobs, cancels superseded PR
  runs, uses a trusted Rust cache policy, and runs the full merge/release-parity
  gate.
  [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)

The v2 design document explicitly intended a changed diff to invalidate only
affected check/review receipts, but the implementation currently keys every
checkpoint to the whole attempt hash. Cross-attempt affected-check reuse has not
landed.
[Harness simplification design](./2026-07-27-harness-simplification-root-cause.md#minimal-command-surface)

## Current measurements

A read-only rollup over the 73 current v2 `evidence.json` attempts found:

| Result | Attempts |
|---|---:|
| `PASSED` | 18 |
| `NEEDS_FIX` | 47 |
| `NEEDS_EVIDENCE` | 8 |

Every one of the 47 `NEEDS_FIX` attempts had all deterministic checks green.
The attempt became non-passing only when a reviewer found a major defect. This
is the strongest evidence that review ordering, not reviewer execution time, is
the primary loop-level defect.

| Check | Runs | p50 | p95 | Total time | Queue wait |
|---|---:|---:|---:|---:|---:|
| `rust-lib` | 70 | 7m14s | 13m27s | 476.9m | 108.3m |
| Playwright | 37 | 6m37s | 7m10s | 246.8m | 0 |
| `tauri-boot` | 36 | 1m23s | 8m28s | 101.9m | 45.3m |
| `perf-contracts` | 28 | 1m30s | 9m17s | 63.8m | 23.8m |
| Angular build | 39 | 6s | 7s | 4.1m | 0 |
| Angular lint | 39 | 3s | 4s | 2.2m | 0 |

The 55 non-passing attempts consumed 367 minutes of Rust checks, 227 minutes of
Playwright, 91 minutes of boot smoke, and 60 minutes of performance checks.
Those minutes are not all automatically avoidable because final reviewers need
real evidence, but a source-first review can reject source-visible defects
before most of that work starts.

Typical reviews are much cheaper:

| Review | p50 |
|---|---:|
| combined Codex review | 61s |
| egress specialist | 33s |
| lock specialist | 38s |

For the final Codex provider attempt, `rust-lib` took 399s: 137s waiting for the
lane, 46s compiling, and 214.5s executing 2,609 tests serially. That split makes
both queue policy and Nextest worth measuring.

## Findings

### 1. Mature systems separate feedback from authority

OpenAI's current Codex automation example generates a patch in a read-only job
and passes it as an artifact to a separate write-authorized PR job. Codex best
practices also separate planning, implementation, testing, and review rather
than treating one model completion as acceptance.
[Codex non-interactive automation](https://learn.chatgpt.com/docs/non-interactive-mode),
[Codex best practices](https://learn.chatgpt.com/guides/best-practices)

GitHub follows the same shape: coding-agent output becomes a PR, review is a
separate lifecycle step, and review effort is risk-tiered. Routine changes use
the fast review level; security-sensitive and cross-service changes use the
deeper level. Re-review can be requested after new pushes.
[GitHub Copilot code review](https://docs.github.com/en/copilot/concepts/agents/code-review),
[Copilot PR lifecycle](https://docs.github.com/en/copilot/tutorials/use-copilot-code-review-across-the-pull-request-lifecycle)

Murmur is already stricter than these systems because its final verdict is
hash-bound. The optimization is to add a cheap, non-authoritative feedback
stage, not relax the authoritative stage.

### 2. Review should reject source defects before execution evidence

Today `verify_task()` runs all planned checks before invoking reviewers.
[`.agents/harness/cli.py`](../../.agents/harness/cli.py#L1750)

Add a first review phase over:

- exact diff and acceptance contract;
- changed-path/risk classification;
- bounded unchanged trust-seam context;
- test names/source and planned commands;
- no claimed runtime result.

The only positive outcome is `READY_FOR_EVIDENCE`. It is not a verdict and is
never included as a substitute for final review. `NEEDS_FIX` or a missing proof
plan stops before Rust/Playwright. After checks pass, fresh final reviewers see
the full immutable evidence bundle and alone can contribute to `PASSED`.

This adds about one minute to the clean one-pass case but can save 7–25 minutes
on a source-visible failed attempt.

### 3. Deterministic checks should form a DAG, not one serial list

The runner currently iterates `plan["checks"]` sequentially.
[`.agents/harness/cli.py`](../../.agents/harness/cli.py#L1784)

Use two bounded chains:

```text
Rust chain: rustfmt/source guards -> clippy parity -> rust-lib
                                      -> tauri-boot/non-duplicate perf claims

Web chain:  ng lint -> ng build -> Playwright
```

Run the chains concurrently. Preserve the existing exclusive FIFO only for
Cargo/rustc-capable steps. Do not run two Cargo compilations concurrently.
Review fan-out remains concurrent after both chains finish.

On a mixed Rust/UI change, the current p50 Rust and Playwright legs alone cost
about 13m51s sequentially. Parallel chains reduce that portion toward the slower
single leg, approximately 7 minutes, before optional boot/performance claims.

GitHub uses the same branch-scoped concurrency principle and supports cancelling
superseded work.
[GitHub Actions concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)

The local runner should similarly cancel an obsolete attempt once the developer
worktree changes, instead of discovering the new diff only after the current
long check exits. The immutable old attempt remains archived, but it cannot
progress toward a receipt.

### 4. Harness Playwright is needlessly half-speed

Harness pins `--workers=1`.
[`.agents/harness/config.json`](../../.agents/harness/config.json#L36)

The canonical CI script pins `--workers=2` and documents why it is safe: specs
are page-side mocked, use a private port, do not reuse a server, and have no
shared state. The macOS runner intentionally stops at two workers.
[`scripts/ci.sh`](../../scripts/ci.sh#L238)

Use the same `workers=2` command in Harness. This is the smallest, lowest-risk
latency patch and requires no new dependency. Benchmark the exact suite before
claiming a percentage improvement.

### 5. Use the Nextest path already proven by CI

CI already installs pinned `cargo-nextest@0.9.98` and `scripts/ci.sh` uses it
when present. The local Harness does not provision it and forces serial
`cargo test -- --test-threads=1`.
[`scripts/ci.sh`](../../scripts/ci.sh#L151),
[`.github/workflows/ci.yml`](../../.github/workflows/ci.yml#L237)

Nextest advertises up to 3x faster execution, supports test inventory,
partitioning, rerun records, and explicit flaky semantics.
[cargo-nextest](https://nexte.st/),
[nextest retries](https://nexte.st/docs/features/retries/)

Run a controlled equivalence spike:

1. provision the exact CI-pinned binary into a runner-owned, checksum-verified
   tool cache rather than installing it globally;
2. compare discovered test inventory against `cargo test -- --list`;
3. run the same Codex-provider test tree with both commands;
4. require zero missing tests and no hidden retry;
5. if retries are ever enabled, use `--flaky-result fail`.

The recent attempt spent 214.5s in serial test execution, so this optimization
targets real time rather than the already-small 46s compile slice.

### 6. Stop rerunning the same Rust performance tests

`perf-contracts.sh` currently reruns the `audio::recorder`, `audio::spill`,
`perf`, and `thermal` subsets even when `rust-lib` has just executed the full
library suite. Preserve the performance-contract claim, but make the combined
plan execute each Rust test once. The supplemental shell syntax checks can
remain a separate cheap step.

This is a narrower and safer win than introducing a general cache: the inputs
and assertions do not change, only duplicate execution is removed.

### 7. Borrow affected hashing carefully

Bazel's safe cache model hashes declared action inputs, command, environment,
and output metadata. Its documentation also warns that undeclared external tools
can produce incorrect hits. Nx selects affected tasks from Git history plus a
project graph, and deliberately falls back broadly for dependency-lock changes.
[Bazel remote caching](https://bazel.build/remote/caching),
[Nx affected](https://nx.dev/docs/features/ci-features/affected),
[Nx task inputs](https://nx.dev/docs/features/cache-task-results)

For Murmur:

- reuse dependency/compiler artifacts across patches now;
- keep semantic reviews and security specialist reviews fresh for every diff;
- keep the current full-diff receipt;
- initially keep check verdict reuse limited to the same exact diff;
- add `check_input_sha256` in shadow mode before enabling any cross-attempt
  deterministic PASS reuse.

The shadow manifest should conservatively include:

- Rust: `src-tauri/**`, workspace Cargo files, `.cargo/**`, sibling protocol
  revision/source when relevant, toolchain identity, command, and bound env;
- Web: `src/**`, `e2e/**`, Angular/TypeScript/ESLint/Playwright configs,
  package manifests/lock, Node/npm/Playwright versions, command, and bound env;
- runtime/performance: both product inputs and the runner-owned smoke/contract
  scripts.

Only after mutation selftests prove that every relevant input invalidates the
key should the runner reuse a deterministic check across attempts. Unknown
inputs or toolchain drift fall back to rerun. Even then, the final receipt binds
the reused evidence record and its recomputed input hash to the new exact diff.

This is the sound version of the v2 design's "invalidate only affected check
receipts" goal. A plain path heuristic or Playwright `--only-changed` is useful
for advisory preflight only, not final evidence.

### 8. Retry only activities, not the whole workflow

Temporal's durable-execution guidance records activity results and retries
failure-prone activities; it explicitly discourages retrying the whole
deterministic workflow because that repeats already-successful work.
[Temporal workflow replay](https://docs.temporal.io/workflows),
[Temporal retry policies](https://docs.temporal.io/encyclopedia/retry-policies)

Harness v2 already has an append-only ledger and exact-attempt checkpoints, so
adding Temporal or LangGraph would add operations without improving Git
provenance. Apply the principle locally:

- `BLOCKED`/timeout reviewer or check -> retry only that activity;
- source finding -> new exact attempt, but keep warm build artifacts;
- unchanged-diff interruption -> resume completed checkpoints;
- changed diff -> never reuse semantic review.

OpenHands' experimental critic similarly bounds iterative refinement to a small
maximum number of rounds, but an LLM critic score is not a trustworthy commit
gate.
[OpenHands Critic](https://docs.openhands.dev/sdk/guides/critic)

### 9. Align local final preflight with CI before pushing

The recent Codex provider passed Harness but failed remote Clippy. Harness's
canonical Rust evidence is `cargo test --lib`; CI additionally runs
`cargo clippy --all-targets -- -D warnings`.
[`.agents/harness/config.json`](../../.agents/harness/config.json#L36),
[`scripts/ci.sh`](../../scripts/ci.sh#L148)

Do not add Clippy to every exploratory repair. Run it in the stable-diff
evidence phase, before the expensive full Rust test, so a warning fails fast
and the compiled artifacts can be reused by the following test command.

There is also a correctness issue adjacent to this latency work: CI configures
Playwright retries but does not pass `--fail-on-flaky-tests`. A test that passes
only after retry can therefore leave the authoritative lane green. Fixing this
does not make the loop faster, but it must accompany any retry optimization.
[Playwright CLI](https://playwright.dev/docs/test-cli),
[`playwright.config.ts`](../../playwright.config.ts#L24)

## Fit with Murmur constraints

- **Lock/egress/protocol:** keep a fresh final specialist Codex review for every
  changed exact diff. Advisory pre-review and caches never satisfy it.
- **Codex only:** all proposed model-assisted phases can use fresh independent
  Codex sessions. No Claude execution is required.
- **Local-first/privacy:** keep source/evidence local except for the existing
  source-code review egress. Never place vault content, transcripts, release DB
  material, credentials, or user data in caches or review packets.
- **macOS:** no cache or mocked E2E substitutes for real TCC,
  ScreenCaptureKit, Touch ID, signing, notarization, or seal round-trip evidence.
- **CI:** the full GitHub `scripts/ci.sh` remains required on the PR/merge SHA.
  Local affected preflight never becomes merge authority.
- **Resource safety:** preserve one Cargo/rustc compile lane. Parallelize the Web
  chain and read-only reviews, not two heavy ML compilations.

## Options and tradeoffs

| Option | Effort | Expected value | Verdict |
|---|---:|---|---|
| Playwright workers 2 + performance-test de-dup + stable-diff Clippy ordering | S | Immediate minutes saved / CI roundtrip avoided | Do first |
| Source-only pre-review before heavy evidence | M | Largest repair-loop reduction | Do |
| Parallel Rust/Web check DAG + obsolete-attempt cancellation | M | ~35–50% successful mixed-pass reduction | Do |
| Pinned local Nextest equivalence spike | S/M | Targets 214s serial Rust execution | Do as measured spike |
| Shadow `check_input_sha256` manifests | M | Enables future affected evidence reuse safely | Do after P0 |
| Cross-attempt deterministic PASS reuse | M/L | High upside, high input-closure risk | Enable only after shadow proof |
| Bounded Codex repair coordinator outside Harness | M | Fewer manual handoffs, not less compute | Optional later |
| Bazel/Pants/Nx or Temporal/LangGraph migration | L/XL | Operational complexity exceeds benefit | Reject |

## Recommendation and first step

Implement in four measured PRs rather than one control-plane rewrite:

1. **Low-risk parity patch:** Harness Playwright `workers=2`; de-duplicate
   `perf-contracts` after `rust-lib`; add timing comparison and make CI flaky
   retries fail the gate.
2. **Feedback-order patch:** advisory source/contract review returning only
   `READY_FOR_EVIDENCE | NEEDS_FIX | NEEDS_EVIDENCE_PLAN`; add RED tests proving
   it cannot mint or contribute to `PASS`.
3. **Scheduler patch:** two check chains, one Cargo lane, concurrent Web lane,
   immediate cancellation when the developer diff changes.
4. **Rust execution spike:** provision pinned Nextest, prove inventory
   equivalence, benchmark cold/warm/one-line-patch scenarios, then decide the
   canonical command.

Success criteria over the next 20 real attempts:

- mixed Rust/UI successful p50 below 12 minutes before final review;
- source-visible `NEEDS_FIX` returned within 2 minutes;
- Cargo queue wait reduced by at least 30%;
- zero stale checkpoint acceptance and zero missing tests;
- every final review and receipt remains bound to the final exact diff;
- no Claude process or review session.

## Open questions

- How much of the 47 review-failed attempts would a source-only reviewer have
  rejected without completed runtime evidence? Measure; do not assume all.
- Will Nextest preserve all Murmur test semantics and resource limits locally?
  CI compatibility is strong evidence, not a local benchmark.
- Can the Cargo lane safely release after compilation while tests execute under
  a separate CPU budget? This may reduce the 177 minutes of observed queue wait,
  but it is a second-stage optimization and needs thermal/RAM measurement.
- Is the broad runtime/performance claim selection on recent feature tasks
  intentional, or are claims being over-requested? Claims should remain honest,
  but unnecessary claims directly add boot/performance minutes.

## Sources

- OpenAI, [Harness engineering](https://openai.com/index/harness-engineering/)
- OpenAI, [Codex best practices](https://learn.chatgpt.com/guides/best-practices)
- OpenAI, [Codex non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)
- GitHub, [Copilot code review](https://docs.github.com/en/copilot/concepts/agents/code-review)
- GitHub, [Copilot PR lifecycle](https://docs.github.com/en/copilot/tutorials/use-copilot-code-review-across-the-pull-request-lifecycle)
- GitHub, [Actions concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)
- Bazel, [Remote caching](https://bazel.build/remote/caching)
- Nx, [Affected tasks](https://nx.dev/docs/features/ci-features/affected)
- Nx, [Task cache inputs](https://nx.dev/docs/features/cache-task-results)
- cargo-nextest, [Overview](https://nexte.st/) and [retry semantics](https://nexte.st/docs/features/retries/)
- Temporal, [Workflow replay](https://docs.temporal.io/workflows) and [retry policies](https://docs.temporal.io/encyclopedia/retry-policies)
- OpenHands, [Experimental Critic](https://docs.openhands.dev/sdk/guides/critic)
- Local implementation:
  [config](../../.agents/harness/config.json),
  [profile derivation](../../.agents/harness/verifier.py#L496),
  [verification lifecycle](../../.agents/harness/cli.py#L1750),
  [check runtime](../../.agents/harness/runtime.py#L2090),
  [CI gate](../../scripts/ci.sh)
