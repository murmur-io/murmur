# Agentic workflow — Murmur (binding)

The implementer never owns the verdict. Use independent, adversarial evidence
for every risky or multi-step change.

## Verifier-only Harness

The Harness is opt-in for risky, multi-step, or operator-requested work:

```text
open -> implement in isolated worktree -> plan -> verify/resume
     -> exact PASS -> commit -> push/PR/CI/merge -> clean
```

It has no implementation-model dispatch and no automatic repair loop. One developer
owns the isolated worktree and declared paths. The runner owns the exact-diff
snapshot, deterministic checks, fresh tool-free reviews, typed probes, receipt,
commit, and cleanup.

Run orchestration from a dedicated standalone driver clone, never a linked
worktree backed by the user's primary `.git`. After `open`, run the task's own
`scripts/agent-harness`; executable protocol drift is a hard refusal.
The Harness cannot certify its own protected control-plane paths. Change those
in a dedicated worktree outside the runner-owned `../.murmur-agent-tasks`
root, for example `../.murmur-control-plane/<task-id>`. Run the complete
control-plane selftests, obtain a fresh independent review, and let the
base-anchored CI gate decide.

```bash
scripts/agent-harness open <task-id> --prompt "<scope>" \
  --owned <path> [--owned <path> ...]
scripts/agent-harness plan <task-id>
scripts/agent-harness verify <task-id>
scripts/agent-harness resume <task-id>  # only when evidence is incomplete
scripts/agent-harness commit <task-id> -m "<message>"
# After push, PR checks, and merge (or explicit archived handoff):
scripts/agent-harness clean <task-id>
```

The behavioral contract cannot prescribe executable proof commands; the
actual-diff plan is the sole command/evidence profile. Keep a committed task
through push, PR creation, and CI. Run `clean` only after merge or an explicit
operator-approved archived handoff.

The current exact changed paths, not caller-provided risk labels, select the
canonical profile. Rust, Angular, UI behavior, and protocol surfaces get their
required checks. Protected harness/control-plane surfaces are refused.
Runtime/performance are explicit `--claim` values. Actual
lock/egress/protocol paths add the mandatory security specialist.

Reviewers are fresh and tool-free, with at most three independent reviews in
parallel. They receive only the runner-built immutable diff/evidence bundle;
they have no filesystem or shell tools. One bounded retry is allowed only for
transient reviewer failure. They may request a typed allowlisted probe; the
runner executes the canonical command and reruns a fresh review bound to that
probe evidence. A MAJOR/BLOCKER, proof gap, unresolved probe, stale diff, or
protocol drift forbids PASS.

Completed exact-attempt green checkpoints survive interruption. State events
are authoritative, projections repair only from those events, and a durable
commit intent recovers a process death after `git commit` without creating a
second commit. `clean --abandon` archives every visible tracked/untracked task
byte before removing the isolated worktree.

`agent/v2/*` is reserved for Harness receipts and every branch-authored
non-merge commit there needs one. The CI verifier retains read-only support for
historical v1 receipt trailers so old Git history remains auditable; there is
no executable v1 lifecycle. No receipted history may return to
`Harness-Lane: B`. Lane B remains only an explicit pre-receipt opt-out on an
ordinary non-v2 `agent/*` branch.

## One shared resource lane

Every agent-issued Cargo/rustc/full-CI process must use the workspace lane
under `../.murmur-agent-tasks/.resources`; it is shared by the primary checkout,
linked task worktrees, and the standalone driver clone:

```bash
scripts/agent-resource-run --chdir src-tauri -- cargo test --lib
scripts/agent-resource-run -- bash scripts/ci.sh
scripts/agent-dev-run -- npm run dev
```

The lane publishes owner PID/task/command/start time and heartbeat immediately.
Waiters report the owner immediately and heartbeat; there is no silent wait.
Long-lived dev stays outside the flock and proxies only child Cargo/rustc
processes through it.

## Adversarial verification

A compiling change is not done. A fresh verifier must try to make its claims
false:

- Run the real selected gates. For Rust inner loops use `cargo test --lib`,
  never bare `clippy --all-targets`.
- Live-reproduce FE/IPC behavior when the change requires it. Drive
  `http://localhost:1420` with a mocked
  `window.__TAURI_INTERNALS__.invoke`, or boot the dev app and inspect
  `/tmp/murmur-dev.log` for an abort-free launch; a synthetic or unit-only gate
  is not runtime proof.
- A bug regression must fail on the unpatched code and pass on the new code. A
  test that also passes before the fix did not capture the bug. Empirical RED
  counts only when a runner-owned artifact performed and recorded it; developer
  prose or a reconstruction is never proof.
- Hunt known shipped failures: seal content loss, sealed-content/asset leaks,
  macOS FFI aborts, stale IPC effects, standalone import cycles, overlay opacity
  bleed, and packaged WebKit CSP style loss.
- Any lock/crypto/content-read change requires the lock security specialist.

The verifier records findings; it never edits the implementation. Repair
belongs to the developer, followed by a new exact-diff verify.

## Trust code, not docs

Repository prose drifts. Confirm every load-bearing claim against the current
file and symbol, and distrust the first read.

**Cite by symbol, not line number.** Commands and storage move among growing
domain modules under `commands/` and `storage/`; search symbols such as
`meeting_is_unlocked` or `visibility_clause` before trusting a prose anchor. A
line citation is only a hint. The sourcing audit is
`docs/research/2026-07-02-claude-setup-audit.md`.

For Claude multi-task programs, use a session-scoped `/goal` that requires the
next manifest action within 60 seconds after the prior stable outcome. Task
scheduling stays outside the verifier.

## Honesty and ownership

Headless checks cannot prove real mic capture, ScreenCaptureKit/TCC, Touch ID,
lock-at-rest, signed-build behavior, notarization, or a real meeting workflow.
Name that boundary explicitly.

Commits are only `QueaT <kgm004a@gmail.com>` with no AI co-author trailers.
Push, PR creation, merge, signing, notarization, and publication are
operator-owned. Never direct-push `murmur`; use a PR. `com.meetnotes.app` is
immutable. No new npm package or crate without explicit approval.
