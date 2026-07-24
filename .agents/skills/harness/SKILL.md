---
name: harness
description: Run a change through the full Murmur harness — isolated worktree, writer, deterministic checks, independent adversarial + risk reviews, hash-bound PASS attestation, guarded commit. Use PROACTIVELY when a change is risky, multi-step, or you want the earned safety net (lock/crypto/egress/protocol, or anything you want independently verified). Skip it for docs, chores, and low-risk edits — those commit normally.
---

# `/harness` — opt-in rigor

Murmur's harness is **opt-in**. Normal commits run freely (only secret-scan and
trunk-push protection are always on). Invoke the harness deliberately, via this
skill, when a change deserves independent verification.

## When to reach for it

- Anything touching the lock model / crypto / secrets / storage / MCP / egress /
  the sharing protocol (the `risk_classification` paths in
  `.agents/harness/config.json`).
- A multi-step feature or refactor where you want a fresh adversarial reviewer to
  try to break the change before it lands.
- Any time you want the hash-bound Definition-of-Done receipt on the commit.

For a docs fix, a chore, a version bump, or a small low-risk edit: **do not use
this** — just commit normally.

## How it works

The switch is physical: the harness runs in an **isolated sibling worktree** that
carries a task manifest. While that task is active, `finish-guard` enforces the
full attestation and the resource lane is required for heavy commands. Your main
`murmur` checkout never has a task, so work there is unconstrained.

## Run it

```bash
# 1. Create the contract + isolated worktree (task_id is positional;
#    --prompt and --owned are required, --owned may repeat for multiple paths)
scripts/agent-harness init <task-id> --kind <bug|feature|refactor|docs|harness> \
  --prompt "<what>" --owned <path> [--owned <path> ...] \
  [--risk <lock|egress|protocol|runtime|ui|performance|release>] \
  [--agent <codex|claude>] [--reviewer <codex|claude>]

# 2. Drive writer → checks → independent reviews → PASS attestation
scripts/agent-harness run <task-id>

# 3. Commit from the attested index (QueaT identity, no AI trailers) and open a PR
scripts/agent-harness commit <task-id> -m "<type>(<scope>): <subject>"
gh pr create -R murmur-io/murmur --base murmur
gh pr merge --merge

# 4. Close the task
scripts/agent-harness close <task-id>
```

## Choosing the writer/reviewer pair

- `--agent` sets the writer vendor (default: `config.json` `default_writer`).
- `--reviewer` sets the reviewer vendor (default: `config.json` `default_reviewer`).
- All four pairs are allowed: `claude→codex`, `codex→claude`, `claude→claude`,
  `codex→codex`. **The shipped default is `claude→claude`** — the harness runs
  entirely on Claude, no Codex dependency.
- Whatever the pair, the reviewer is ALWAYS a fresh, independent session with no
  writer context — so same-vendor is still a real adversarial review, not
  self-grading, and the implementer never owns the verdict.
- **Trade-off:** same-vendor loses cross-model-family diversity (Codex and Claude
  catch different failure modes). Prefer a cross-vendor pair when that diversity
  matters most (e.g. lock/crypto/egress changes).

## Verify the harness itself

```bash
bash .claude/hooks/selftest.sh
scripts/agent-harness selftest --ci
scripts/agent-config-audit --ci
```
