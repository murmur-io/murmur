---
name: pr-program
description: >-
  Run a multi-PR Murmur program through verifier-only Harness v2: one local
  mutation at a time, exact-diff verification, remote CI, durable continuity,
  and lossless cleanup.
---

# /pr-program

Harness verifies one task. The program queue remains an orchestrator concern.
V2 has no writer and no repair loop: an implementation agent edits the isolated
worktree, while the runner alone owns checks, reviews, evidence, and PASS.

## Program rules

- Use the dedicated standalone `.murmur-agent-driver`; never the primary
  checkout or a linked driver worktree.
- Keep at most one task locally editing or verifying. Parallelize only read-only
  mapping and research.
- Cargo and full CI always use the shared resource lane.
- Keep a simple dependency-ordered manifest outside the harness.
- Establish a session `/goal`: after every stable outcome, execute the next
  manifest action within 60 seconds. Saying "I will start the next task" is not
  progress.
- Default to finishing and merging one PR before opening its dependent task.

## Before each task

```bash
cd ../.murmur-agent-driver
git fetch origin murmur
git switch --detach origin/murmur
scripts/agent-harness doctor
```

If fetch fails or `open` warns that it is falling back to local HEAD, stop. Do
not knowingly start a program task from a stale base.

The contract describes observable behavior and invariants only. Do not demand
commands or self-reported evidence. Mention only files or specifications
available in the committed base, or include the necessary requirement directly.

## Per-PR lifecycle

```bash
scripts/agent-harness open <task-id> \
  --kind <bug|feature|refactor|docs> \
  --prompt "<behavior and invariants>" \
  --owned <path> [--owned <path> ...] \
  [--claim runtime] [--claim performance] \
  [--reviewer codex|claude]
```

Edit only the printed task worktree and declared scope. Then, from that
worktree:

```bash
scripts/agent-harness plan <task-id>
scripts/agent-harness verify <task-id>
scripts/agent-harness status <task-id>
```

The derived plan is the executable evidence profile. Never compensate for a
missing canonical check by putting a command in prose.

The reviewer is tool-free. When deterministic evidence is missing, it may
request only an allowlisted canonical typed probe ID. The runner executes an
accepted probe against the same immutable verification snapshot and binds its
result to the next review. The operator never supplies or pastes an arbitrary
command. `--claim runtime` and `--claim performance` add planned checks to the
evidence profile; a claim is not itself a probe.

State handling:

- `NEEDS_FIX`: repair the worktree, then run `verify`; the changed diff creates
  a fresh attempt.
- `NEEDS_EVIDENCE`, `PAUSED_RETRYABLE`, `INTERRUPTED`, or `STALE`: run
  `resume`; completed green checkpoints are reused.
- `PASSED`: do not edit the diff; commit it through the harness.

```bash
scripts/agent-harness commit <task-id> \
  -m "<type>(<scope>): <subject>"
git push -u origin agent/v2/<task-id>
gh pr create -R murmur-io/murmur --base murmur \
  --head agent/v2/<task-id>
```

Wait for required CI, then merge through the PR. Never rebase or amend the
attested task commit. If trunk moved, only a conflict-free automatic merge of
`origin/murmur` may ride after it; conflicts require a fresh task and receipt.

After merge:

```bash
scripts/agent-harness clean <task-id>
```

For abandonment:

```bash
scripts/agent-harness clean <task-id> --abandon
```

`clean --abandon` archives tracked and untracked bytes. Never manually delete a
task directory or branch.

## End of program

```bash
scripts/agent-harness doctor
scripts/agent-harness metrics --limit 50
```

Require every manifest row to be merged or explicitly archived, no open task
worktrees, a clean doctor result, and remote `murmur` containing every accepted
PR.

## Protected control-plane exception

Harness v2 refuses to certify its own protected harness, hook, rule, skill, and
learnings surfaces. Those changes use the separate externally anchored v1
bootstrap with `--kind harness` and `seal-prepared`; this exception must never
be used to bypass v2 for ordinary product work.

## Do not resurrect v1 habits

Do not use manual worktrees, `init/run`, custom `--check`, inferred `--risk`,
manual reviewer workflows, receipt trailers, `rm -rf`, or local rebase of an
attested commit. Protected control-plane changes are the sole bootstrap
exception described above.
