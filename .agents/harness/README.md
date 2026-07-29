# Murmur development harness

The harness is Murmur's vendor-neutral verification control plane. It is
separate from the application's AI features and model evaluations.

Harness v2 is a Phase-1 shadow candidate. It is deliberately thin and
verifier-only, but it does not become the repository default until historical
defect replay and real-task shadow budgets pass:

```text
open -> edit isolated worktree -> plan exact diff -> verify/resume
     -> exact PASS evidence -> commit -> push/PR/CI/merge -> clean
```

There is no harness-owned writer and no automatic repair loop in v2. The
developer or delegated implementation agent edits the isolated worktree. The
runner owns only isolation, deterministic evidence, fresh tool-free reviews,
receipt verification, the exact commit, and cleanup. The implementer never
owns the verdict.

Legacy v1 remains executable for existing tasks and is the only supported
bootstrap for protected harness/control-plane changes. Its `init`, `run`,
`seal-prepared`, `verify-attestation`, `close`, `reap`, `gc`, and `eval`
commands dispatch unchanged to `task_runner.py`.

## Shadow-candidate v2 workflow

Run multi-task orchestration from a dedicated, long-lived **standalone driver
clone**, not the user's primary checkout and not a linked worktree. The
standalone `.git` is the driver's isolation boundary: Claude may create task
worktrees without receiving write access to the user's primary `.git`. Create
it once with ordinary Git:

```bash
git fetch origin murmur
git clone --local --no-hardlinks --no-checkout . ../.murmur-agent-driver
git -C ../.murmur-agent-driver remote set-url origin https://github.com/murmur-io/murmur.git
git -C ../.murmur-agent-driver fetch origin murmur
git -C ../.murmur-agent-driver switch --detach origin/murmur
cd ../.murmur-agent-driver
```

Do not replace this with `git worktree add`: a linked driver stores its Git
metadata in the user's primary checkout, outside Claude's project sandbox.
The checked-in Claude policy grants only the sibling
`../.murmur-agent-tasks` root needed for task worktrees. Cargo/full-CI and
runtime-port reservations live under that same narrow root, so the standalone
driver and the user's primary checkout still share one visible resource lane.

Open a candidate task from that driver:

```bash
scripts/agent-harness open attachment-loss \
  --kind bug \
  --prompt "Fix attachment loss after closing a note" \
  --owned src-tauri/src/storage/attachment_store.rs \
  --owned src-tauri/src/commands/attachments.rs \
  --owned src-tauri/src/commands/tests/attachment_tests.rs
```

`open` starts from a committed base, creates
`../.murmur-agent-tasks/v2/<task-id>/meetnotes`, and prints the exact worktree.
If that committed tree has the pinned `murmur-server` Cargo path dependency,
`open` also creates and prints an exact-revision local shared clone at
`../.murmur-agent-tasks/v2/<task-id>/murmur-server`. Its Git metadata stays
inside the task root, so focused Rust commands work immediately without
mutating the sibling repository's worktree registry.
The primary checkout's dirty bytes are never copied. Edit only the printed
worktree and stay inside the declared `--owned` paths. After opening, invoke the
task worktree's own `scripts/agent-harness`; the runner refuses a caller whose
executable protocol differs from the task-pinned protocol.

Plan and verify from the isolated task worktree:

```bash
scripts/agent-harness plan attachment-loss
scripts/agent-harness verify attachment-loss
scripts/agent-harness status attachment-loss
```

`plan` is optional but useful for inspection. `verify` always rebuilds the plan
from the current exact binary diff before using evidence. If infrastructure
pauses or a typed probe is collected, continue with:

```bash
scripts/agent-harness resume attachment-loss
```

Completed green checks for the same attempt are reused byte-for-byte.
Timed-out or environmentally blocked checks rerun. Any diff, tree, plan, or
protocol change creates a different attempt and invalidates stale evidence.

The task description is a behavioral acceptance contract. It names outcomes
and invariants, not commands for the writer to run or report. The plan derived
from the actual changed paths is the sole executable check profile.

After `PASSED`, commit, push, and create the PR. Keep the task worktree until
the PR has merged or the operator explicitly accepts an archived handoff:

```bash
scripts/agent-harness commit attachment-loss \
  -m "fix(attachments): preserve files when closing a note"
git -C ../.murmur-agent-tasks/v2/attachment-loss/meetnotes \
  push -u origin agent/v2/attachment-loss
gh pr create -R murmur-io/murmur --base murmur \
  --head agent/v2/attachment-loss \
  --title "fix(attachments): preserve files when closing a note" \
  --body "What changed and how the exact diff was verified"
# After the PR merges:
scripts/agent-harness clean attachment-loss
```

`commit` stages only the declared scope, re-verifies the exact PASS evidence,
requires `QueaT <kgm004a@gmail.com>`, rejects receipt/co-author injection, and
writes one commit with v2 receipt trailers. A durable intent makes the command
resumable if the process dies after `git commit` but before the receipt/state
write: rerunning the exact message finalizes the existing commit and never
creates another one.

`clean` verifies a clean committed task, archives the exact tip, then removes
only its isolated worktree and branch. An uncommitted task requires
`clean --abandon`; before removal it archives every Git-visible tracked and
untracked byte through a private index.

## Plans are derived, not claimed

The changed path set selects canonical commands from `config.json`; callers
cannot replace them with weaker commands:

- Rust source or manifests: `cargo test --lib`.
- Angular source: lint and production build; behavior `.ts`/`.html` also gets
  Playwright.
- Sharing protocol or `.murmur-server-revision`: client Rust plus the pinned
  sibling `murmur-protocol` test.
- Harness/control-plane changes are refused by v2. During shadow they use the
  externally anchored v1 `--kind harness` plus `seal-prepared` workflow.
- Runtime and performance checks: only explicit `--claim runtime` or
  `--claim performance`.
- Actual lock, egress, and protocol paths: the combined review plus the
  corresponding cross-vendor security specialist.

The protocol hash includes the runner, canonical checks, prompts, schemas,
wrappers, receipt verifier, hooks, config audit, runtime smoke implementation,
and CI wiring. Changing any of those invalidates prior plan evidence.

V2 refuses every path protected by `config.json`, including its own runner,
hooks, schemas, CI, rules, and skills. A task may not weaken the code that
judges it.

Reviews run in fresh tool-free model sessions, at most three in parallel. Each
receives only the runner-built immutable diff/evidence bundle and has no
filesystem or shell tools. A transient reviewer failure gets one bounded retry
with both attempts recorded. A review may request only a typed, allowlisted
probe; the runner executes the canonical command, records it, and requires a
fresh review bound to that probe evidence. `PASS` is rejected when any
MAJOR/BLOCKER, proof gap, or unresolved probe remains.

## Crash and contention behavior

The append-only state event is authoritative; `state.json` is a repairable
projection. Runner checkpoints are written atomically before progress advances.
`resume` therefore recovers from SIGKILL without repeating green evidence.

Plan, verify/resume, commit/guard, and clean mutations are serialized by one
task lock. Cargo and full-CI work share one workspace resource lane under
`../.murmur-agent-tasks/.resources`, across the primary checkout, linked task
worktrees, and the standalone driver clone:

```bash
scripts/agent-resource-run --chdir src-tauri -- cargo test --lib
scripts/agent-resource-run -- bash scripts/ci.sh
scripts/agent-dev-run -- npm run dev
```

The lane publishes owner PID, task, command, start time, and heartbeat
immediately. Waiters report that owner immediately and then heartbeat; there is
no silent 30-second wait. Long-lived dev supervision stays outside the lane and
wraps only its child Cargo/rustc processes.

Runtime claims run `scripts/harness-runtime-smoke` as a preflight before an
expensive reviewer dispatch. It uses isolated ports/home/process groups and
separate warm/cold budgets. A headless smoke still does not prove Touch ID,
ScreenCaptureKit/TCC, real capture, notarization, or signed-build behavior.

## Evidence and receipts

V2 task artifacts live under:

```text
.git/agent-harness/v2/tasks/<task-id>/
├── task.json
├── events.jsonl
├── state.json
├── attempts/<attempt-id>/
│   ├── plan.json
│   ├── protocol.json
│   ├── diff.patch
│   ├── checks/
│   ├── probes/
│   ├── reviews/
│   └── evidence.json
├── commit-intent.json
└── commit.json
```

The local evidence verifier binds the contract, base, exact binary diff, tree, plan, protocol,
checks, probes, fresh reviewer invocation metadata/logs, findings, telemetry,
and degradation provenance. The remote
`scripts/verify-harness-attestation` cannot reconstruct private local evidence;
it recomputes the commit parent and exact diff and checks strict v1/v2 trailer
consistency rather than accepting trailer presence alone.

Remote receipt policy is monotonic. `agent/v2/*` is reserved: every
branch-authored non-merge commit there requires a v2 receipt. A receipted
history may upgrade v1 -> v2, but it may never downgrade v2 -> v1 or return to
`Harness-Lane: B`. Lane B is only a deliberate pre-receipt opt-out on an
ordinary non-v2 `agent/*` branch. Renaming a branch does not escape receipt
coverage once its history contains one.

The receipt verifier's deterministic selftest builds real temporary Git
histories for v1, v2, mixed ancestry, merge topology, duplicates, aliases,
unknown versions, amendments, cherry-picks, renamed files, unattested commits,
catch-up smuggling, reserved v2 branches, Lane-B conflicts, and downgrade
attempts. CI runs this suite before expensive builds, on both `pull_request`
and `merge_group`.

Remote policy auditing has two deliberately separate scopes. Pull-request
runs lend GitHub's ordinary read-only token only to
`scripts/agent-remote-audit --public` and may report `PASS_MERGE_SCOPE`: every
merge-blocking rule visible to that token passed, while admin-only controls
remain explicitly `MONITOR_ONLY`. A `merge_group` run executes the
deterministic evaluator selftest and receipt gate, but no live GitHub audit.
Privileged monitoring runs only on `schedule` or `workflow_dispatch` at
`refs/heads/murmur`, using `MURMUR_REMOTE_AUDIT_TOKEN`. The audit script is
repository code and necessarily receives the selected token; `scripts/ci.sh`
clears every token variable immediately afterward, before later repository
checks. The expected split is declared in
`.agents/harness/remote-policy.json`.

## V1 compatibility and import

Finish a valid v1 `PASSED`/`COMMITTED` task in v1. To adopt a nonterminal v1
task without rerunning its writer:

```bash
scripts/agent-harness import-v1 <task-id>
```

Import is byte-preserving and idempotent. It records the source contract and
degraded provenance, reconstructs a missing worktree only from exact archived
Git evidence, and never fabricates PASS. If exact bytes cannot be recovered,
the imported task becomes history-only `NEEDS_EVIDENCE`. Adopting a prior v1
PASS requires explicit `--invalidate-pass`.

While v1 remains the bootstrap for protected control-plane paths, its checks
still run in fail-closed macOS Seatbelt profiles; Playwright receives a
runner-reserved `MURMUR_E2E_PORT`. Writer/reviewer vendors remain selectable
with `--agent`/`--reviewer`, and lock/egress/protocol work escalates a
same-vendor reviewer unless explicitly overridden. `reap` archives the exact
bytes of terminal failed/blocked/closed work before removing its worktree;
`gc` applies that lifecycle to stale tasks.

Historical markerless v1 receipts retain their original, narrower provenance
field set so honestly earned archives remain verifiable. New v1 contracts are
policy-2 and cannot run through that path. This is a consistency receipt, not
a signature: deliberately rewriting both runner-owned contract and receipt
artifacts is outside its stated threat model.

## Operational telemetry

The append-only ledgers for both generations can be rolled up without parsing
multi-megabyte raw model logs:

```bash
scripts/agent-harness metrics
scripts/agent-harness metrics --limit 50 --json
```

`--limit` selects the most recently active task generations by their latest
valid event timestamp. The report includes task/status counts, model
invocations, observed cost and turns, model/check/review durations with
nearest-rank p50/p90, retries, timeouts, and malformed-ledger counts. Every
optional total is paired with coverage (`available`, `missing`, `invalid`);
missing historical telemetry is never imputed as zero. A successful
`model-process-exit` plus its matching `model-invocation` is counted once, while
each bounded retry remains a separate invocation.

## Control-plane verification

```bash
python3 .agents/harness/v2_selftest.py
python3 .agents/harness/metrics_selftest.py
scripts/verify-harness-attestation --selftest
bash .codex/hooks/selftest.sh
scripts/agent-config-audit --ci
scripts/agent-harness selftest --ci
```

`selftest` runs both generations. `doctor` audits dependencies, both schema
families, stale state/lock projections, ghost tasks, orphan worktrees, and
cleanup debt. Local selftests prove the control plane's deterministic behavior;
they do not certify application behavior or remote branch enforcement.

Hooks are fast defense-in-depth. GitHub's required `gate (full ci.sh — release
parity)` remains the remote merge boundary. Push, PR creation, merge, signing,
notarization, and publication remain operator-owned actions.

## Program continuity and cutover

The verifier schedules one task only. For a Claude multi-PR session, set a
session-scoped `/goal` that requires the next manifest action within 60 seconds
after the prior task reaches a stable state. Do not add a repository-wide Stop
hook or put a program queue inside the receipt state machine.

V2 remains a candidate until shadow evidence includes the historical MAJOR
corpus, at least ten representative real tasks, the declared kill/429/timeout/
occupied-port/lane/trunk faults, and measured p50/p90 budgets. Only a later
cutover change may call it the default or retire v1's writer/repair topology.
