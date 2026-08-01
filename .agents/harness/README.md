# Murmur development harness

The Harness is an opt-in, verifier-only control plane for risky or multi-step
changes. It creates one isolated worktree, derives checks and reviews from the
exact diff, records resumable evidence, writes the guarded commit, and archives
the task during cleanup.

It does not dispatch an implementation model and has no automatic repair loop.
The developer edits the worktree; a fresh reviewer owns the verdict.

```text
open -> edit isolated worktree -> plan -> verify/resume
     -> PASS -> commit -> push/PR/CI/merge -> clean
```

## Daily workflow

Run orchestration from the standalone driver clone
`../.murmur-agent-driver`, not the user's primary checkout:

```bash
scripts/agent-harness open attachment-loss \
  --kind bug \
  --prompt "Fix attachment loss after closing a note" \
  --owned src-tauri/src/storage/attachment_store.rs \
  --owned src-tauri/src/commands/attachments.rs
```

`open` prints the isolated task worktree. Implement only there and only in the
declared paths. Then use that worktree's checked-in runner:

```bash
scripts/agent-harness plan attachment-loss
scripts/agent-harness verify attachment-loss
scripts/agent-harness status attachment-loss

# Only if verification paused or evidence is incomplete:
scripts/agent-harness resume attachment-loss

# Only after PASS:
scripts/agent-harness commit attachment-loss \
  -m "fix(attachments): preserve files when closing a note"
```

Keep the task through push, PR checks, and merge. Afterwards:

```bash
scripts/agent-harness clean attachment-loss
```

To abandon a task, use `clean <task-id> --abandon`. Cleanup first archives every
Git-visible task byte and only then removes that task's worktree and branch.

## What is automatic

The current changed paths select canonical checks and reviews:

- Rust source/manifests: `cargo test --lib`.
- Angular source: lint and build.
- Browser behavior: Playwright.
- Sharing protocol: client and pinned server protocol tests.
- Lock, egress, and protocol paths: mandatory specialist review.
- Runtime and performance: explicit `--claim runtime|performance`.

The behavioral prompt cannot add shell commands. The derived plan is the sole
executable evidence profile. Reviewers are fresh, read-only, and tool-free.
They may request only a typed, allowlisted probe that the runner executes.

`review_authority` in `config.json` decides which review can forbid a PASS. The
three risk specialists are `blocking`. The `combined` generalist is `advisory`:
it still runs, and its findings, proof gaps, and probe requests are still
recorded in the receipt, but on any plan that keeps another gate they no longer
gate the verdict and no longer spend a probe execution — see
`docs/research/2026-08-01-reviewer-corpus-measurement.md`. Any unconfigured or
unknown review kind is blocking.

Demotion removes a gate; it must never remove the last one. Every PASS names at
least one gate that could have refused it, so `verifier.gating_review_kinds`
skips the demotion for a plan that derived no deterministic check and no
configured blocking review — docs-only, asset-only, and landing-only diffs,
whose only planned review is the generalist. There the generalist still gates,
spends its probe, and can still refuse. The receipt gate re-derives that same
set from the exact paths and the attested config, so a re-hashed `PASSED` on an
ungated plan is refused by the rule that produced it.

Findings from a demoted review are recorded in `evidence.advisory_findings`,
projected into task state, printed by `status`, carried in the `verify` status
JSON, and counted in the PASS reason, which then reads `all blocking checks and
reviews passed; N advisory finding(s) recorded (M MAJOR/BLOCKER)` instead of
claiming every review passed.

Green checkpoints for an unchanged exact diff survive interruption.
`NEEDS_FIX` means edit the worktree and verify the new diff.
`NEEDS_EVIDENCE`, `PAUSED_RETRYABLE`, and `INTERRUPTED` resume without throwing
away completed evidence.

## Protected control-plane changes

The Harness cannot certify changes to its own protected files. For
`.agents/harness`, hooks, rules, skills, CI, or receipt policy:

1. create a dedicated worktree outside the runner-owned
   `../.murmur-agent-tasks` root, for example
   `../.murmur-control-plane/<task-id>`;
2. run the complete control-plane selftests;
3. obtain a fresh independent review;
4. land through the base-anchored GitHub CI gate.

This is an explicit trust-boundary exception, not a weaker self-receipt.

## Commands

```text
open      create the isolated task
plan      print and bind the exact-diff evidence profile
verify    run or resume checks and fresh reviews
resume    continue missing/retryable evidence
status    inspect state and lock ownership
commit    create the exact PASS receipt commit
clean     archive and close/abandon the task
doctor    audit dependencies, ghosts, and orphan worktrees
metrics   summarize current Harness event ledgers
selftest  run lifecycle, fault, and metrics tests
```

There is no executable Harness v1. `scripts/verify-harness-attestation` retains
read-only support for historical v1 commit trailers so old Git history remains
auditable.

## Self-verification

```bash
scripts/agent-harness selftest --ci
scripts/verify-harness-attestation --selftest
bash .codex/hooks/selftest.sh
scripts/agent-config-audit --ci
```

These prove the control plane, not Murmur product behavior. Real runtime,
signed-build, privacy, and content-loss claims still need their corresponding
application evidence.
