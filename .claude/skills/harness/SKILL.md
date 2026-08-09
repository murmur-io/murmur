---
name: harness
description: Verify a risky or multi-step Murmur change through the verifier-only Harness — isolated worktree, exact-diff plan, deterministic checks, fresh adversarial and risk reviews, crash-resumable PASS receipt, guarded commit, and lossless cleanup. Use proactively for lock, crypto, egress, protocol, or any change needing independent verification. Skip it for docs, chores, and low-risk edits.
---

# `/harness` — opt-in verifier-only rigor

Murmur's harness is opt-in. Use it when a change deserves an independently
earned, hash-bound verdict. Ordinary low-risk commits remain outside it.

The Harness does not dispatch an implementation model or repair code. You or the assigned
implementer edits one isolated worktree; the runner derives and verifies the
exact diff. The implementer never owns PASS.

## Use it for

- Lock, crypto, secrets, gated content reads, storage, MCP, egress, or sharing
  protocol changes.
- Multi-step features/refactors where a fresh reviewer should try to break the
  result.
- Any change that needs an exact-diff receipt before commit.

Skip it for a small docs edit, chore, metadata bump, or clearly low-risk
mechanical change unless the operator explicitly wants the receipt.

## Required lifecycle

Run orchestration from a dedicated standalone driver clone, never the user's
primary checkout or a linked driver worktree. After `open`, change into the
printed task worktree and use that
worktree's runner so the executable protocol equals the pinned protocol.

```bash
# 1. Open the contract and isolated worktree.
scripts/agent-harness open <task-id> \
  --kind <bug|feature|refactor|docs|harness> \
  --prompt "<what must be true>" \
  --owned <path> [--owned <path> ...] \
  [--claim <runtime|performance>] \
  [--reviewer <codex|claude>]

# 2. Implement only in the printed worktree and declared scope.

# The prompt is behavioral acceptance only. It must not demand that the developer
# run or report commands; the derived plan is the sole executable evidence.

# 3. Inspect the exact derived plan, then verify.
scripts/agent-harness plan <task-id>
scripts/agent-harness verify <task-id>
scripts/agent-harness status <task-id>

# 4. Continue only missing/retryable evidence when needed.
scripts/agent-harness resume <task-id>

# 5. Commit the exact PASS. Push and create/merge the PR before cleanup.
scripts/agent-harness commit <task-id> \
  -m "<type>(<scope>): <subject>"

# 6. After merge or an explicit archived handoff, clean the isolated task.
scripts/agent-harness clean <task-id>
```

The plan, not the caller, chooses canonical checks from changed paths. Rust,
Angular behavior, and protocol surfaces get their required deterministic
gates. Runtime/performance require explicit claims. Actual
lock/egress/protocol paths add the required cross-vendor specialist. The
Harness refuses protected harness/control-plane paths. Change those in a
dedicated worktree outside the runner-owned `../.murmur-agent-tasks` root,
for example `../.murmur-control-plane/<task-id>`. Run the complete
control-plane selftests, obtain a fresh independent review, and rely on the
base-anchored CI gate.

Reviewers are fresh, tool-free sessions. They receive only the runner-built
immutable diff/evidence bundle and have no filesystem or shell tools. A
transient review gets one bounded retry. Reviewers may request only typed
allowlisted probes, which the runner executes canonically before a fresh
review. No gating PASS is valid with a MAJOR/BLOCKER, unresolved probe, stale
diff, or changed protocol hash. A gating reviewer's residual proof gap stays
recorded and named in the PASS reason but does not override its own PASS;
evidence required to decide the contract must produce `BLOCKED`. Advisory
review artifacts remain recorded but do not vote or spend a probe execution.

If verification returns:

- `NEEDS_FIX`: repair the worktree yourself, then rerun `verify`.
- `NEEDS_EVIDENCE` after `verify` collected a typed gating probe: use `resume`
  to run fresh reviews over its bound output. For a bare reviewer `BLOCKED`
  without a probe, unchanged `resume` reuses the same checkpoint; add the
  missing proof to the diff and run `verify`, or abandon and reopen if the
  contract needs a new claim.
- `PAUSED_RETRYABLE` or `INTERRUPTED`: use `resume`; green checkpoints survive.
- `PASSED`: use only `commit`; keep the task through push, PR, CI, and merge,
  then `clean`.

For abandonment, run `clean <task-id> --abandon`. It archives every visible
tracked/untracked byte before removing only that task's worktree.

## Historical receipts

There is no executable v1 lifecycle. The CI receipt verifier retains read-only
support for historical v1 trailers so old Git history stays auditable.
`agent/v2/*` is reserved for current Harness receipts, and no receipted history
may return to `Harness-Lane: B`.

For a Claude multi-task program, establish a session `/goal` requiring the next
manifest action within 60 seconds of the previous stable outcome. Scheduling
does not belong to the verifier.

## Verify the harness

```bash
python3 .agents/harness/v2_selftest.py
scripts/verify-harness-attestation --selftest
bash .codex/hooks/selftest.sh
scripts/agent-config-audit --ci
scripts/agent-harness selftest --ci
```

These certify deterministic control-plane behavior, not Murmur product
behavior. Push, PR, merge, signing, notarization, and publication remain
operator-owned.
