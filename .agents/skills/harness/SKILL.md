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
# 1. Create the contract + isolated worktree
scripts/agent-harness init --kind <feature|refactor|docs|harness> --title "<what>"

# 2. Drive writer → checks → independent reviews → PASS attestation
scripts/agent-harness run

# 3. Commit from the attested index (QueaT identity, no AI trailers) and open a PR
scripts/agent-harness commit
gh pr create -R murmur-io/murmur --base murmur
gh pr merge --merge

# 4. Close the task
scripts/agent-harness close
```

The default vendor pair is Claude writer → Codex reviewer (the only supported
reversal is Codex writer → Claude reviewer). The implementer never owns the
verdict.

## Verify the harness itself

```bash
bash .claude/hooks/selftest.sh
scripts/agent-harness selftest --ci
scripts/agent-config-audit --ci
```
