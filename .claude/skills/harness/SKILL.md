---
name: harness
description: Shadow-test a risky or multi-step Murmur change through the verifier-only Harness v2 candidate — isolated worktree, exact-diff plan, deterministic checks, fresh adversarial and risk reviews, crash-resumable PASS receipt, guarded commit, and lossless cleanup. Use proactively for lock, crypto, egress, protocol, or any change needing independent verification. Skip it for docs, chores, and low-risk edits.
---

# `/harness` — opt-in verifier-only rigor

Murmur's harness is opt-in. Use it when a change deserves an independently
earned, hash-bound verdict. Ordinary low-risk commits remain outside it. V2 is
still a shadow candidate, not the repository default.

Harness v2 does not dispatch a writer or repair code. You or the assigned
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

# The prompt is behavioral acceptance only. It must not demand that the writer
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
lock/egress/protocol paths add the required cross-vendor specialist. V2
refuses protected harness/control-plane paths; use the externally anchored v1
`--kind harness` and `seal-prepared` flow for those during the shadow.

Reviewers are fresh, tool-free sessions. They receive only the runner-built
immutable diff/evidence bundle and have no filesystem or shell tools. A
transient review gets one bounded retry. Reviewers may request only typed
allowlisted probes, which the runner executes canonically before a fresh
review. No PASS is valid with a MAJOR/BLOCKER, proof gap, unresolved probe,
stale diff, or changed protocol hash.

If verification returns:

- `NEEDS_FIX`: repair the worktree yourself, then rerun `verify`.
- `NEEDS_EVIDENCE`: use `resume`; it runs only the missing evidence.
- `PAUSED_RETRYABLE` or `INTERRUPTED`: use `resume`; green checkpoints survive.
- `PASSED`: use only `commit`; keep the task through push, PR, CI, and merge,
  then `clean`.

For abandonment, run `clean <task-id> --abandon`. It archives every visible
tracked/untracked byte before removing only that task's worktree.

## Compatibility

Legacy-only `init`, `run`, `seal-prepared`, `verify-attestation`, `close`,
`reap`, `gc`, and `eval` still dispatch to v1; generation-aware `status` and
`commit` preserve existing v1 tasks. Finish a valid v1 PASS in v1. Adopt a
nonterminal v1 diff with `import-v1`; import preserves source artifacts and
never fabricates PASS.

Receipt policy is monotonic. `agent/v2/*` is reserved for v2 receipts. A
legacy receipted history may upgrade v1 -> v2, but cannot downgrade v2 -> v1
or return to `Harness-Lane: B`. Lane B is valid only as a pre-receipt opt-out
on an ordinary non-v2 `agent/*` branch.

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
