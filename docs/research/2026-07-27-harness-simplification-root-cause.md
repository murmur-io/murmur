<!-- Generated 2026-07-27 via /research (murmur-researcher fan-out). Pricing/funding/version = point-in-time. -->
# Research: Harness root cause and simplification

> **Implementation update — 2026-07-28.** The recommended verifier-only
> architecture is now on `murmur`: PR #496 landed the v2 engine, PR #499
> activated the generation-aware CLI and resumable evidence lifecycle, and PR
> #501 made verification snapshots self-contained under Seatbelt. Guarded
> receipts, the base-revision CI verifier, `merge_group` wiring, doctor/metrics,
> lossless cleanup, and the sherpa archive fix from PR #478 are all merged. A
> real interrupted/resumed docs task preserved its exact diff, recovered a stale
> lock, reached `NEEDS_FIX` instead of a terminal loss, and exposed the snapshot
> bug subsequently fixed by #501. V2 remains opt-in while the broader real-task
> and high-risk shadow budgets below are incomplete; v1 remains only for
> existing tasks and externally anchored changes to protected control-plane
> paths. The diagnosis below is the decision record, not current operator
> instructions. Current commands live in `.agents/harness/README.md` and
> `.agents/skills/harness/SKILL.md`.

## TL;DR / Verdict

The v1 harness is not suitable as Murmur's default feature-development loop.
The field reports are directionally correct: the independent review primitive works,
but the operations surrounding it make good work slow, fragile, and sometimes
practically unrecoverable.

The root cause is not "too much rigor" in the abstract. It is **architectural
coupling**: one synchronous state machine currently acts as all of these at once:

1. a model-driven developer and repair loop;
2. a worktree manager;
3. a semantic-risk classifier and test planner;
4. a global build-resource scheduler;
5. an evidence collector;
6. a multi-reviewer adjudicator;
7. a commit/PR provenance system.

Those responsibilities fail differently, but the harness collapses most failures
into one terminal `BLOCKED` state. A missing proof artifact, HTTP 429, reviewer
timeout, occupied runtime port, interrupted process, stale instruction hash, test
failure, and a genuinely unsafe implementation can all have effectively the same
lifecycle consequence.

The recommended direction is **Harness v2 as a thin, resumable verifier over an
existing diff**, not another patch series on the current automatic writer loop:

- development happens normally in an isolated worktree;
- the harness derives the actual changed surfaces from the diff;
- runner-owned checks execute once for that diff;
- one fresh reviewer combines specification and adversarial review;
- only lock/egress/protocol changes add a concurrent specialist reviewer;
- every non-PASS result is resumable in the same worktree;
- an exact-diff evidence receipt is still required before commit;
- GitHub CI remains the integration/release-parity truth.

This preserves the differentiating value that caught real defects while deleting
the machinery responsible for most lost time.

At the diagnosis snapshot, the separate symptom, "Claude says it will start the
next task and then stops," had two operational causes:

- **between turns:** there is no durable program queue or continuation mechanism in
  the harness; `run` accepts one task, and Claude auto mode does not begin another
  turn by itself;
- **inside a command:** the Cargo lane waits silently, so an invoked command can
  look frozen.

Program continuation remains deliberately outside the verifier and should use a
session-scoped Claude `/goal` (or, only if later justified, a tiny external
queue). The silent wait is fixed in v2-era resource tooling: lane ownership and
heartbeats are visible. Neither concern belongs in the trust receipt.

**Current operating decision (2026-07-28):** do not open new v1 feature tasks.
Use v2 selectively for changes that need an independently earned receipt; ordinary
low-risk work stays outside the opt-in harness. Finish/import existing v1 tasks,
and use externally anchored v1 only when a change owns protected harness or agent
control-plane files that v2 must refuse to self-certify.

Confidence: **high**. The conclusion is supported by current code, the local task
corpus, live process/state evidence, GitHub state, and four independent research
angles.

## What Murmur already has

The harness contains a good verification kernel. It should be retained rather than
discarded:

- the runner, not the writer, stages and derives the owned diff
  (`.agents/harness/task_runner.py:1035-1065`);
- deterministic checks run before review, and the runner verifies that review did
  not mutate the tree (`.agents/harness/task_runner.py:3238-3379`);
- reviewers are fresh and receive the exact diff plus check evidence
  (`.agents/harness/task_runner.py:2619-2651`,
  `.agents/harness/task_runner.py:3385-3431`);
- commit trailers bind the parent and normalized diff, and CI recomputes them
  (`.agents/harness/task_runner.py:4524-4556`,
  `scripts/verify-harness-attestation:262-331`);
- specialist lock/egress/protocol reviews already exist
  (`.agents/harness/config.json:26-41`);
- the implementer is not allowed to certify its own result
  (`.agents/harness/prompts/implementer.md:7-14`).

The reports are also right that this kernel has delivered real value. Independent
reviewers caught defects that deterministic tests missed, including failures in
orchestrator-authored code. Removing independent review would solve the wrong
problem.

### Implementation size at the diagnosis snapshot

At the 2026-07-27 diagnosis snapshot, the primary v1 harness/control-plane
scripts were already approximately 10.9k lines before counting all selftests and
surrounding skills:

- `.agents/harness/task_runner.py`: 6,999 lines;
- `.agents/harness/hook_guard.py`: 2,108 lines;
- `.agents/harness/resource_policy.py`: 703 lines;
- `scripts/agent-resource-run`: 397 lines;
- `scripts/harness-runtime-smoke.py`: 211 lines;
- plus configuration, schemas, prompts, wrappers, and CI glue.

Those counts are historical, not a current trunk inventory: #496/#499 added the
separate v2 engine and its fault/selftests, and v1 also changed during the
bootstrap. The architectural conclusion is unchanged: adding resume, mutation
testing, prompt NLP, reviewer retries, parallel review, re-attestation, program
scheduling, telemetry, and more terminal states directly to v1 would preserve the
wrong abstraction and expand its invalid-state space.

### Corrections to the supplied reports

The reports describe real incidents, but several claims need to be reconciled with
the current checkout and live GitHub state.

| Claim | Current finding |
|---|---|
| `--base` defaults to `HEAD` | Fixed in code: init now fetches and defaults to `origin/murmur` (`task_runner.py:3987-4037`). CLI help and the local `pr-program` prose are stale and still say `HEAD` (`task_runner.py:6911`). |
| Init never prunes worktrees | Both repositories are now pruned at init (`task_runner.py:700-713`, `4127-4131`). A discoverable archival `clean` path and routine GC are still missing. |
| A timed-out writer always loses its tree | Fixed in current code: a timed-out writer can proceed as degraded (`task_runner.py:2468-2492`). Earlier degraded work can still be laundered out of the final attestation by a later clean round. |
| Any trunk movement invalidates a harness PR | Too broad. The remote verifier explicitly permits a legitimate catch-up merge from the base branch (`scripts/verify-harness-attestation:78-128,291-331`). PR #477's attestation gate passed; its failing lane was web/E2E. The local `verify`/`close` lifecycle remains overly tied to the original base and one-parent shape. |
| Harness-owned files have no supported workflow | Current code has `--kind harness` plus `seal-prepared` (`task_runner.py:3982`, `4217-4319`). The workflow is obscure, but O14 is not an open capability gap. |
| A `BLOCKED` task's bytes are literally destroyed | Too strong in current code. Work generally remains staged, and `reap` archives Git-visible bytes under a hidden ref (`task_runner.py:4604-4805`). The real defect is that recovery is obscure, non-resumable, and easy to do incorrectly. |
| PR #478 fixed the sherpa archive issue and is merged | This was false at the 2026-07-27 snapshot, but became true on 2026-07-28: PR #478 is merged and trunk contains the `.agents/harness/checks/` marker. |
| The runner restored/deleted primary-checkout control-plane edits | Not supported by the inspected code. Staging/reset logic targets the isolated task worktree (`task_runner.py:1035-1065`). The incident needs separate reproduction; another agent or trunk update is currently more plausible. |

These corrections do not change the verdict. They narrow the redesign to the
failures that still exist.

## Findings

Every finding in this section describes Harness v1 at the 2026-07-27 diagnosis
snapshot unless a later status paragraph explicitly says that v2 landed the
repair. Current operator instructions live in `.agents/harness/README.md` and
the Harness skill, not in this historical diagnosis.

### 1. The strongest observed v1 root cause was stale state without liveness

At the 2026-07-27 snapshot, the local v1 task corpus contained 122 task
directories:

| Persisted state | Count |
|---|---:|
| `REAPED` | 34 |
| `BLOCKED` | 31 |
| `CLOSED` | 17 |
| `INITIALIZED` | 14 |
| `FAILED` | 12 |
| `CHECKING` | 9 |
| `RUNNING` | 4 |
| `PASSED` | 1 |

All 27 persisted non-terminal tasks (`INITIALIZED`, `CHECKING`, `RUNNING`) had no
`run.lock`. Only seven still had a worktree; twenty had neither a live runner nor
the worktree implied by their state. These are **ghost tasks**, not merely old task
records.

The current `ux-p4-settings-ia-20260727` task was a concrete reproduction:

- state remained `CHECKING`, round 2;
- round-2 writer, vocabulary, lint, and build evidence had completed;
- Playwright output continued but no final check event was persisted;
- there was no task lock and no harness runner process;
- an outer Claude process was observed sleeping and polling `status` for that ghost
  instead of starting or resuming work.

The code explains the persistence:

- `status` prints persisted JSON and does not reconcile it with `run.lock`
  (`task_runner.py:4322-4337`);
- cancellation can bypass the `HarnessError` path that writes a terminal state,
  while `finally` removes the lock (`task_runner.py:3532-3548,6985-6999`);
- another `run` refuses a prior non-`INITIALIZED` state and converts it to terminal
  `BLOCKED` rather than resuming (`task_runner.py:3158-3193`).

This is the highest-confidence explanation for the observed "polling forever"
failure. A persisted phase without a live owner is being treated as progress.

Confidence: **high**. Sources: current task store and code paths above.

### 2. Risk classification is incorrectly used as the test planner

The current model conflates four independent decisions:

1. which language/build surface changed;
2. whether the content is security-sensitive;
3. whether a behavioral runtime/performance claim is being made;
4. which evidence is required for the receipt.

`classify_risks()` classifies broad owned paths, and
`required_risk_evidence()` maps those semantic flags directly to commands
(`task_runner.py:548-630`). The configuration maps:

- lock/egress/protocol to `rust-lib`;
- runtime to `tauri-boot`;
- performance to `perf-contracts`
  (`.agents/harness/config.json:35-50`).

`transcribe/**` is classified as runtime and performance
(`config.json:92-118`). Therefore a Rust string-rendering fix can schedule a
full application boot and performance contracts while omitting `cargo test --lib`.
This exactly confirms P0-1.

Adding another risk glob is not the right fix. **Test selection must begin with the
actual changed language surface**, while risk classification should decide only
specialist review and non-suppressible security evidence.

Recommended separation:

- changed Rust source -> `rust-lib` minimum;
- changed Angular source -> `ng-lint` + `ng-build`; Playwright when behavior/UI is
  involved;
- changed shared protocol -> client and server protocol tests;
- actual sensitive lock/egress/protocol path -> specialist review;
- runtime boot/performance -> explicit claim/profile, never inferred merely from a
  directory name.

Confidence: **high**. Sources: code/config and the R1/R2 task evidence.

### 3. The prose contract can demand evidence that no authorized role can create

The writer is explicitly told that runner-owned checks are authoritative
(`implementer.md:7-14`). Claude reviewers are launched with only
`Read,Grep,Glob`, no Bash (`task_runner.py:2363-2372`). Codex reviewers use a
read-only workspace profile (`task_runner.py:2292-2344`).

At the same time, the spec reviewer must reject missing evidence. A prompt that
says "run clippy" or "prove RED-before-GREEN" therefore creates a structurally
unsatisfiable contract:

- the writer's claim is not authoritative;
- the runner did not schedule the command;
- the reviewer is not allowed to run it;
- the reviewer correctly blocks.

This confirms P0-2, P0-3, O11, and O13. It also explains why repeated rounds with
correct code could never satisfy the contract.

Do not fix this with prompt scanning or a generic mutation engine:

- prose should contain behavior and acceptance criteria only;
- executable evidence should be a typed profile owned by the runner;
- init/verify should print the fully resolved check/reviewer plan;
- missing evidence should yield resumable `NEEDS_EVIDENCE`;
- a reviewer that needs a targeted empirical probe should request an allowlisted
  runner-owned check, not receive unrestricted Bash.

For ordinary bug fixes, the first slice should require a regression test plus the
appropriate language suite. A true RED proof can later be an explicit
`regression-proof` profile that applies only the test patch to an ephemeral base.
If it cannot be reproduced safely, record a `proof_gap`; never substitute writer
prose as evidence.

Claude's sandbox supports exact write paths rather than disabling isolation, so a
bounded runner-owned probe is compatible with the security boundary:
[Claude sandbox documentation](https://code.claude.com/docs/en/sandboxing).

Confidence: **high**.

### 4. `BLOCKED` is an overloaded terminal state

`BLOCKED` is terminal (`task_runner.py:44-46`). Current paths use it for:

- interrupted/prior incomplete runs;
- writer self-report;
- environment probes;
- deterministic check failure after repair exhaustion;
- reviewer uncertainty or failure;
- model errors and timeouts;
- instruction drift;
- harness exceptions
  (`task_runner.py:3158-3548`).

There is no `resume`, `retry`, `amend`, or `salvage` subcommand in the CLI
(`task_runner.py:6881-6973`).

This turns recoverable facts into destructive lifecycle outcomes. The correct
model is an append-only attempt ledger in a persistent task:

```text
OPEN -> VERIFYING -> PASS -> COMMITTED -> CLOSED
            |
            +-> NEEDS_FIX
            +-> NEEDS_EVIDENCE
            +-> PAUSED_RETRYABLE
            +-> INTERRUPTED
```

Only `CLOSED` and explicit `ABANDONED` should be terminal. Every other state keeps
the worktree and can be resumed from the last durable phase, provided the diff hash
still matches.

Confidence: **high**.

### 5. Writer self-report has too much lifecycle authority

The runner exits before staging or deterministic checks whenever the writer
returns `status=blocked` (`task_runner.py:3197-3228`).

The local corpus contains 18 writer-terminal blocks:

- 9 treated inability to write `.git/index.lock` or stage as fatal, even though the
  runner owns staging;
- 6 treated occupied runtime/check infrastructure as the writer's blocker;
- 3 represented genuine scope or implementation uncertainty.

Thus 15/18 were boundary confusion rather than evidence that the produced tree was
invalid. The runner should inspect and stage a valid owned diff even when the
writer's narrative says "blocked." Writer status is useful feedback, not a trust
verdict.

Confidence: **high**. Source: aggregate local model/event logs.

### 6. Reviews are duplicated, serial, and can skip the adversarial review

Every default task requires both spec and adversarial review
(`config.json:26-29`). They run in a serial loop and short-circuit on the first
non-PASS (`task_runner.py:3381-3438`).

Observed across the local corpus:

- 49 review rounds;
- 20/49 ran spec review but never reached adversarial review;
- the longest review round ran five reviewers sequentially for 35.6 minutes;
- recorded reviewer time totals 6.2 hours.

This is both slower and weaker than the intended invariant: the adversarial
reviewer does not always get to try to break the change.

The default topology should be:

1. one fresh reviewer combining acceptance/spec coverage and adversarial
   correctness;
2. one additional specialist only for actual lock/egress/protocol paths;
3. independent reviewers run concurrently, capped at 2-3;
4. aggregate the worst verdict; never short-circuit.

This is a hypothesis that must be evaluated against the historical caught-defect
set before the separate spec process is deleted. Independence means "fresh and
separate from the writer," not "two sequential generalists."

Confidence: **high** for current cost/topology, **medium** until the combined
reviewer replay is measured.

### 7. Two current trust-plane defects can overstate PASS

These must be fixed even if v1 is otherwise frozen.

#### Earlier degraded writer rounds disappear

`create_attestation()` reads `writer_runs[-1]`
(`task_runner.py:2919-2931`). If round 1 timed out and round 2 completed cleanly,
the attestation can omit degradation even though the final tree contains work from
round 1. Repair prompts and reviewer context likewise focus on the current round.

The receipt must aggregate provenance from **every contributing attempt**.

#### PASS can contain unresolved MAJOR/BLOCKER findings

The review schema permits any verdict/findings combination
(`.agents/harness/schemas/review.schema.json:7-24`). Runtime logic primarily
consumes `verdict`, and the attestation review summary omits the findings array
(`task_runner.py:2955-2974`).

The invariant must be:

```text
PASS => no unresolved BLOCKER or MAJOR findings
```

All findings must be included in the evidence bundle. This should auto-block now,
not merely print a warning later; the historical corpus already contains
MAJOR/BLOCKER findings inside PASS verdicts.

Confidence: **high**.

### 8. Model and infrastructure failures are classified as verdict failures

A reviewer process error or timeout is fatal; writer timeout has a degraded path,
but reviewers do not (`task_runner.py:2466-2511`). The parser retains only model
and session ID, discarding useful terminal reason, cost, turns, and usage
(`task_runner.py:2189-2219`). Four observed HTTP 429 results became terminal
blocks.

Anthropic classifies rate limits and 5xx errors as transient and its official SDKs
retry them with exponential backoff, honoring `retry-after` when present:
[Claude API errors](https://platform.claude.com/docs/en/api/errors).

The harness should:

- perform one bounded retry for 429/5xx/network failure and reviewer timeout;
- honor `retry-after`;
- then enter `PAUSED_RETRYABLE`, preserving the diff and prior evidence;
- never rerun the writer or completed checks solely because a reviewer API call
  failed;
- record terminal reason, duration, cost, turns, and usage in the attempt ledger.

Confidence: **high**.

### 9. The global Cargo lane should remain, but waiting must be observable

Serializing the heavy Cargo build on this Mac is reasonable. The defect is that
both lane implementations poll silently and reveal ownership only after the full
deadline (`scripts/agent-resource-run:223-264`,
`task_runner.py:1690-1728`). Check evidence is written only after the command
completes, so historical lane wait and command runtime cannot be separated.

Every resource acquisition should immediately emit:

```text
waiting for cargo lane:
  owner_pid=<pid>
  task=<task-id>
  command=<short command>
  since=<timestamp>
  heartbeat=<timestamp>
```

Repeat at 30-second intervals. Separate at least `cargo` and `runtime-ports`
resources; do not serialize unrelated read-only work behind one opaque mutex.

Lane contention can explain an invoked command appearing frozen. It cannot explain
Claude ending a turn without issuing the next command.

Confidence: **high**.

### 10. "Claude says next and stops" is not a harness task transition

The harness wrapper executes one Python command, and `run` accepts exactly one task
ID (`scripts/agent-harness:1`, `task_runner.py:6918-6925`). There is no program
queue, `next`, or durable dispatcher. `.claude/settings.json` configures
`PreToolUse` and `PostToolUse`, but no Stop continuation
(`.claude/settings.json:35-54`).

Claude's official documentation is explicit:

- auto mode approves tool use within a turn but does not start another turn;
- `/goal` keeps working across turns until a measurable condition is met;
- `/goal` is session-scoped and supported from Claude Code 2.1.139;
- non-interactive default text output can itself look stuck, so
  `--output-format stream-json --verbose` should be used.

Source: [Claude Code `/goal`](https://code.claude.com/docs/en/goal).
The installed Claude Code version observed during this research was 2.1.220.

Immediate recommendation for a multi-PR program:

```text
/goal Continue until every item in <program manifest> is in a stable state
(merged, needs-user-authority, or explicitly abandoned), and surface the next
task action within 60 seconds of the prior item stabilizing.
```

Do not install a permanent repository-wide Stop hook as the first fix. A
session-scoped goal is easier to bound and inspect. If programs must survive
process/session loss, add a tiny external `program.json` queue after task liveness
is repaired; do not put multi-PR orchestration back into the verifier.

Confidence: **high** for the missing mechanism, **medium** for the exact reason a
particular historical Claude turn ended because the outer transcript was not
available.

### 11. The primary-checkout orchestrator has an avoidable blast radius

Current hook protections are activated when the working directory belongs to an
active task. The primary checkout normally does not, so branch surgery and
unserialized operations there do not receive the same task guard
(`hook_guard.py:469-490,643-705`).

At the same time, the instruction hash reads live control-plane files from that
checkout (`task_runner.py:633-697`). Normal harness development or trunk movement
can therefore invalidate an in-flight task's control-plane fingerprint.

The driver should run in a dedicated long-lived orchestrator worktree and should
never merge locally:

- create PRs and merge server-side;
- fetch the resulting trunk;
- leave the user's primary checkout and live dev app untouched;
- snapshot a small immutable verification protocol bundle per attempt instead of
  hashing mutable live source throughout a long task.

Confidence: **high** for the code architecture; the reported O17 deletion event
was not reproduced.

### 12. Runtime/performance checks are behavior claims, not directory defaults

The current `runtime` inference can schedule `tauri-boot` for broad Rust changes.
Port occupancy is checked only in the check phase, after the writer, and the cold
boot uses a fixed 240-second wait (`scripts/harness-runtime-smoke.py:119-145`).
This explains both the late "installed Murmur owns the port" block and the cold
build timeout/warm rerun pass.

V2 should:

- require runtime/performance profiles explicitly;
- run port and prerequisite preflight before any model work;
- distinguish cold and warm boot budgets;
- keep the correct rule that unknown processes are never killed;
- treat occupied user-owned ports as `PAUSED_RETRYABLE`, not a verdict.

Confidence: **high**.

### 13. The measured v1 cost was incompatible with a default development loop

The 2026-07-27 aggregate local snapshot contained:

- 274 recorded model invocations;
- writers: 162 invocations, 19.4 model-hours, median 227 s, p90 1,138 s;
- reviewers: 112 invocations, 6.2 model-hours, median 117 s, p90 428 s;
- deterministic checks: 430 executions, 13.0 hours;
- `rust-lib`: 6.53 hours;
- Playwright: 2.71 hours.

The 176 Claude invocations with terminal billing metadata total approximately
$975.81 and 7,885 turns. The UX P0-P4 attempts alone consumed approximately
$224.44, 1,861 Claude turns, and at least 6.1 serialized hours; only P0 completed
the harness lifecycle.

These are not clean product KPIs: the corpus includes harness development,
selftests, retries, and earlier task generations, and Codex cost is not included.
They are nevertheless decisive operational evidence that the current loop cannot
be the default way to build features.

Confidence: **high** for the aggregate, with the scope caveat above.

## Target design: Harness v2

### System boundary

```text
developer/orchestrator worktree
  writes and iterates normally
            |
            v
thin verifier on exact diff
  surface checks -> fresh review(s) -> evidence receipt
            |
            v
normal commit / PR
            |
            v
GitHub CI or merge queue on current trunk
```

The verifier should not own feature decomposition, automatic repair, multi-PR
scheduling, local merging, or publication.

### Responsibility map

| Component | Owns | Does not own |
|---|---|---|
| Developer agent | implementation, targeted inner-loop tests, fixing findings | final verdict |
| Worktree helper | create/list/archive/clean, optional sibling repo only when touched | checks or review |
| Verification runner | actual-diff classification, canonical checks, evidence, resource lanes | implementation decisions |
| Fresh reviewer | acceptance + adversarial correctness; targeted probe requests | arbitrary shell, staging, mutation |
| Security specialist | lock/egress/protocol review when actual sensitive paths changed | general duplicate review |
| GitHub CI / merge queue | integration with latest trunk, full release-parity gate | local implementation |
| Operator | push, PR, merge, credentials/signing/publication | routine verifier internals |

### Two profiles, not an open-ended risk matrix

#### Standard

Derived from actual changed files:

- Rust source: `cargo test --lib` once per final diff;
- Angular source: `ng lint` + `ng build`;
- behavioral UI change: relevant Playwright smoke;
- shared protocol: client + server protocol tests;
- one fresh combined spec/adversarial reviewer.

The full `scripts/ci.sh` remains a PR/release-parity gate, not a repair-round
inner loop.

#### High-risk

Standard profile plus one concurrent cross-vendor specialist for actual:

- lock/crypto/content visibility;
- cloud egress/redaction/ledger;
- shared protocol/wire format.

`runtime` and `performance` are explicit optional claim profiles. They are not
semantic risk flags.

### Minimal command surface

```text
harness open <task> --prompt-file <path> --owned <path>...
harness plan <task>
harness verify <task>
harness resume <task>
harness status <task>
harness commit <task> -m <message>
harness clean <task>
```

`verify` is idempotent for an unchanged diff. A changed diff invalidates only the
affected check/review receipts. No automatic writer or repair rounds exist in the
verifier.

If a model-assisted repair convenience is retained, it should be a separate
command that operates on the same worktree and prior findings:

```text
harness amend <task>
```

It must not be part of the trust state machine.

### Evidence model

Each attempt snapshots:

- base and head/diff identity;
- actual changed files and resolved profile;
- small immutable protocol bundle version;
- a runner-owned, shallow, self-contained Git repository for the exact planned
  tree, with non-empty or unsafe object alternates rejected;
- check IDs, commands, exit codes, duration, stdout/stderr digests;
- reviewer identity/session, verdict, all findings, and proof gaps;
- model degradation from every contributing attempt;
- retry/infra classification and resource-wait duration.

The commit receipt remains presence-and-consistency evidence, not a signing
system:

```text
Harness-Version: 2
Harness-Task: <id>
Harness-Verdict: PASS
Harness-Base: <actual parent>
Harness-Diff-Sha256: <exact normalized diff>
Harness-Evidence-Sha256: <checks + reviews + protocol bundle>
Harness-Writer-Degraded: <optional aggregate>
```

Do not replace the exact diff hash with `git patch-id`: Git documents patch IDs as
stable under line-number and whitespace changes, which is useful for advisory
equivalence but not exact security/provenance identity:
[git patch-id](https://git-scm.com/docs/git-patch-id).

### Moving trunk

The current remote receipt verifier already supports a legitimate catch-up merge.
The local lifecycle should stop insisting that the worktree remain at the original
single-parent tree after verification.

For sustained multi-PR programs, GitHub's merge queue is the cleaner integration
solution: it tests queued changes against the latest target without requiring each
author to update and rerun the branch manually. PR #499 added the distinct
`merge_group` workflow trigger and its local attestation self-check; only
enabling and configuring the repository merge queue remains optional future
operator work, not a Harness v2 prerequisite:
[GitHub merge queue documentation](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue).

## Fit with Murmur constraints

### Local-first and privacy

No hosted scheduler, telemetry SaaS, or durable-execution service is needed.
Task state, logs, diffs, and receipts remain local. Existing source-code review
egress does not expand to note/audio/user data.

### Obsidian-native, SQLite-canonical, provider seam

Unaffected. This is a development control-plane redesign. It must not create new
product data stores or AI-provider paths.

### macOS and heavy local builds

Keep OS sandboxing and the serialized Cargo lane. Add visibility and bounded
runner-owned probes instead of widening reviewers to unrestricted Bash or `.git`.
Runtime/TCC/ScreenCaptureKit evidence remains explicit Mac evidence rather than a
directory-name inference.

### Lock model and cloud egress

Keep mandatory specialist review for actual lock/crypto/content-read and egress
paths. Simplification must reduce duplicate general review, not security-specific
coverage.

### CI honesty

Local verification is a change-specific evidence set. `scripts/ci.sh` and GitHub
remain full integration/release gates. Since PR #499, `scripts/ci.sh` runs
`scripts/verify-harness-attestation --selftest`, GitHub verifies receipts from the
base revision, and the workflow handles `merge_group`. This closes the local
self-check and merge-queue wiring gaps identified in the original snapshot.

### One-person operation

Avoid Temporal/Restate/DBOS, a database-backed scheduler, signing infrastructure,
approval workflows, or an always-on service. Borrow checkpoint/retry semantics,
not their operational footprint.

## Options and tradeoffs

### Option A: patch all open v1 defects

Effort: **L**
Risk: **very high**
Unlocks: incremental compatibility, but preserves the monolith.

This would add more states and heuristics to an already large control plane:
resume, salvage, mutation testing, prompt analysis, retry policy, parallel review,
re-attestation, queueing, telemetry, and expanded selftests. It fixes incidents
one at a time without correcting responsibility boundaries.

**Reject.**

### Option B: thin resumable verifier v2

Effort: **M**
Risk: **medium**
Unlocks: normal development speed while preserving independent, exact-diff
verification.

This was the recommended option and is now built beside v1. It reuses the proven
diff/check/receipt primitives without carrying the automatic writer/repair
topology into v2.

### Option C: retire the harness entirely

Effort: **S**
Risk: **medium/high**
Unlocks: immediate speed and simplicity.

Normal feature branches, project CI, a fresh adversarial reviewer, and specialist
security review would still be much better than unreviewed work. However, this
loses enforced exact-diff receipts and makes it easier to accidentally bypass the
verdict, which already happened operationally.

Use this only as the **temporary low-risk operating mode** while Option B is built,
not as the target.

## Recommendation and first step

### Decision

Option B was selected and landed in PRs #496 and #499. Do not revive the proposed
23-defect patch list as 23 independent v1 fixes. The remaining work is shadow
validation, migration, and eventual deletion of obsolete v1 topology.

### Phase 0: safety bridge and freeze — substantially landed

The diagnosis proposed freezing new v1 low/normal tasks and making only the
changes needed to stop false PASS and work loss while v2 was built:

1. any changed `src-tauri/src/**` schedules `rust-lib` — **landed in v2**;
2. aggregate degraded provenance across every contributing attempt — **landed
   in v2**;
3. enforce `PASS => no unresolved MAJOR/BLOCKER` — **landed in v2**;
4. detect stale non-terminal state from missing/dead `run.lock` and resume from
   the existing diff/check cursor — **landed in v2**;
5. one bounded retry for 429/reviewer timeout, then `PAUSED_RETRYABLE` —
   **landed in v2**;
6. visible lane owner within two seconds and every 30 seconds — **landed in the
   shared resource tooling**;
7. land or replace the then-open #478 sherpa marker fix (merged 2026-07-28).

The related `--prompt-file`, runtime preflight, and lossless archival `clean`
capabilities landed in v2 where applicable. They are not a mandate to broaden
the compatibility-only v1 state machine. Prompt NLP, a generic mutation engine,
and more inferred risk flags remain intentionally absent.

### Phase 1: build v2 beside v1 — landed in #496/#499

Use a separate metadata namespace and dual receipt support. Reuse:

- worktree/diff derivation;
- canonical check execution;
- fresh reviewer adapters;
- exact receipt verification.

Do not reuse:

- automatic writer/repair loop;
- serial spec + adversarial topology;
- terminal `BLOCKED`;
- prose command requirements;
- runtime/performance path inference;
- live mutable instruction hashing;
- unconditional sibling-server worktree creation;
- PR/program orchestration.

### Phase 2: adversarial shadow — in progress

Before replacing v1:

1. replay the historical set of caught MAJOR defects through the combined reviewer;
2. run at least ten real tasks: two Rust, two Angular, one mixed, and at least one
   lock/egress/protocol task;
3. fault-inject HTTP 429, reviewer timeout, kill during a check, occupied ports,
   lane contention, and trunk movement;
4. prove no scenario reruns the writer or loses the diff;
5. compare defects caught, not merely green selftests.

Evidence accumulated by 2026-07-28:

- a real docs task was interrupted during review, then `resume` repaired its
  stale lock without changing the bound diff or discarding the interrupted log;
- the resumed reviewer returned `NEEDS_FIX` and requested typed probes instead
  of turning missing evidence into a terminal task;
- the requested `harness-v2-selftest` probe exposed a verification snapshot that
  borrowed objects from the standalone driver; PR #501 replaced it with a
  self-contained snapshot and added direct/inherited Seatbelt, reconstruction,
  exact-tree, and scoped-cleanup regressions;
- task `continuity-research-refresh-v2` preserved the interrupted diff; the
  corrective control-plane task
  `harness-v2-selfcontained-probe-v4-20260728` carried the independently
  reviewed #501 receipt;
- the externally anchored `harness-v2-selftest`, `hook-selftest`,
  `receipt-selftest`, and `config-audit` checks passed under the real check
  sandbox. Exact historical assertion counts are deliberately omitted here:
  task evidence binds outcomes and log hashes, not parsed count fields.

This is meaningful operational evidence, but it is not the full ten-task corpus
and does not yet cover the required Rust, Angular, mixed, and high-risk task
distribution.

### Phase 3: cut over and delete — pending shadow budgets

- finish already `PASSED`/`COMMITTED` tasks with v1;
- import live/stale worktrees into v2 as `OPEN` or `NEEDS_EVIDENCE` without
  rerunning writers;
- retain v1 read-only verification/close support for 30 days;
- archive, then explicitly GC selected old tasks;
- delete the v1 automatic writer/repair and serial-review machinery after the
  shadow budgets pass.

The migration rule from that snapshot remains: archive and classify the legacy
task store before GC; never bulk-delete it merely because persisted state is
stale. Current counts must come from `agent-harness doctor`, not this document.

### Success budgets

Measure over a rolling 20-task window:

- preflight and resolved plan visible in <=15 s, excluding a bounded remote fetch;
- lane owner visible in <=2 s and repeated every 30 s;
- zero diffs lost or writers rerun because of timeout, 429, occupied port,
  instruction drift, failed check, missing evidence, or trunk movement;
- zero terminal infrastructure failures;
- non-runtime verification p50 <=10 min, p90 <=20 min;
- high-risk verification p90 <=30 min;
- end-to-end non-runtime task p90 <=45 min;
- >=90% of tasks reach `PASS`, `NEEDS_FIX`, or `NEEDS_EVIDENCE` without operator
  cleanup;
- 100% of PASS commits have a recomputable exact-diff receipt;
- combined reviewer catches the historical MAJOR set at least as well as the
  current two-general-reviewer topology;
- with `/goal` active, the next program action begins within 60 s of the prior
  task reaching a stable state.

## Open questions / not verified

1. The exact outer Claude transcript where it announced R2 and stopped was not
   available. The missing continuation mechanism and live ghost polling were
   verified; the exact turn-ending cause remains unknown.
2. The reported deletion/restoration of uncommitted control-plane files was not
   reproduced and is not explained by the inspected runner paths.
3. A combined spec/adversarial reviewer must be tested on the historical defect
   corpus before the second general reviewer is removed.
4. The optimal warm/cold runtime timeout needs measurement on the actual Mac.
5. The typed reviewer probe broker and its no-arbitrary-shell boundary are
   implemented and worked in the interrupted/resumed docs task. Broader real
   Rust and high-risk runs remain outstanding; unrestricted reviewer Bash is
   still not recommended.
6. Merge queue configuration itself was not changed in this research; the
   workflow now has the required `merge_group` trigger after #499.
7. The local task corpus spans harness development and selftests, so its aggregate
   cost/state counts are operational evidence, not a clean production success
   rate.

## Sources

### Repository and local evidence

- `.agents/harness/task_runner.py:44-46,548-697,1035-1065,1298-1320,1690-2025,2189-2219,2292-2372,2466-2511,2619-2651,2919-3023,3142-3548,3699-3948,3982-4319,4322-4337,4524-4805,4889-5070,6881-6999`
- `.agents/harness/cli.py` and
  `.agents/harness/v2_{selftest,fault_selftest}.py`
- `.agents/harness/config.json:26-50,67-125`
- `.agents/harness/prompts/implementer.md:5-14`
- `.agents/harness/schemas/review.schema.json:7-24`
- `.agents/harness/hook_guard.py:286-330,469-490,643-705`
- `scripts/agent-resource-run:223-264,356-368`
- `scripts/harness-runtime-smoke.py:119-145`
- `scripts/verify-harness-attestation:20-27,78-128,262-331`
- `.github/workflows/ci.yml:33-44,73-111`
- `.claude/settings.json:35-54`
- `.git/agent-harness/tasks/*/{state.json,events.jsonl,logs/*.jsonl}` aggregate
  and `ux-p4-settings-ia-20260727` task evidence, inspected 2026-07-27
- [Murmur PR #477](https://github.com/murmur-io/murmur/pull/477)
- [Murmur PR #478](https://github.com/murmur-io/murmur/pull/478)
- [Murmur PR #481](https://github.com/murmur-io/murmur/pull/481)
- [Murmur PR #496](https://github.com/murmur-io/murmur/pull/496)
- [Murmur PR #499](https://github.com/murmur-io/murmur/pull/499)
- [Murmur PR #501](https://github.com/murmur-io/murmur/pull/501)

### External primary sources

- [Claude Code: Keep Claude working toward a goal](https://code.claude.com/docs/en/goal)
- [Claude Code hooks reference](https://code.claude.com/docs/en/hooks)
- [Claude Code sandbox configuration](https://code.claude.com/docs/en/sandboxing)
- [Claude API errors and retries](https://platform.claude.com/docs/en/api/errors)
- [GitHub merge queues](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
- [Git worktree](https://git-scm.com/docs/git-worktree)
- [Git patch-id](https://git-scm.com/docs/git-patch-id)
- [AWS Builders' Library: timeouts, retries, and backoff with jitter](https://aws.amazon.com/builders-library/timeouts-retries-and-backoff-with-jitter/)
