# `.claude/` — Murmur's Claude Code control surface

Claude Code is one adapter to the vendor-neutral development harness in
`.agents/harness/`; it is not a separate source of workflow truth.

| Path | Purpose |
| --- | --- |
| `../CLAUDE.md` | Autoloaded project charter and rule index |
| `settings.json` | Project permissions, sandbox policy, and hook wiring |
| `rules/` | Binding Rust, Angular, lock, and agent-loop rules |
| `agents/` | Thin specialist role prompts |
| `skills/` | Claude-facing mirrors of shared executable runbooks |
| `hooks/` | Claude adapters for the canonical hook guard |
| `learnings/` | Curated recurring lessons and append-only run journals |

The active development loop is:

```text
task contract -> sibling worktree -> writer -> deterministic checks
              -> fresh spec/adversarial/risk reviews -> bounded repair
              -> hash-bound attestation -> guarded commit -> required remote CI
```

Run it with `scripts/agent-harness`; task evidence is stored once under the
shared Git common directory at `.git/agent-harness/tasks/<task-id>/`. Legacy
`.claude/tmp/` verdicts and trace helpers are historical evidence only and have
no authority over commits.

The finish guard is fail-closed. It accepts only a runner-created PASS bound to
the exact staged diff, active instructions, dependency revisions, green checks,
and independent review sessions. `scripts/agent-config-audit --ci` prevents the
Claude and Codex adapters from silently drifting.

Quick verification:

```bash
scripts/agent-harness doctor
scripts/agent-harness selftest --ci
scripts/agent-harness eval selftest
scripts/agent-config-audit --ci
```

Local hooks are defense in depth, not a remote security boundary. Merge safety
ultimately requires GitHub to make the macOS `CI` check required on `murmur`.
Inspect that read-only with `scripts/agent-remote-audit`.
