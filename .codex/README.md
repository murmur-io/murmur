# `.codex/` — Murmur's Codex control surface

Codex is one adapter to the vendor-neutral development harness in
`.agents/harness/`; it is not a separate source of workflow truth.

| Path | Purpose |
| --- | --- |
| `../AGENTS.md` | Autoloaded project charter and rule index |
| `config.toml` | Project-scoped concurrency bounds plus the least-privilege read-only reviewer profile |
| `rules/` | Binding Rust, Angular, lock, and agent-loop rules |
| `agents/` | Thin specialist role prompts |
| `hooks.json` + `hooks/` | Codex wiring/adapters for the canonical hook guard |
| `learnings/` | Curated recurring lessons and append-only run journals |
| `../.agents/skills/` | Shared executable runbooks |

The active development loop is:

```text
task contract -> isolated worktree -> developer edit -> exact-diff plan
              -> deterministic checks -> fresh combined/risk reviews
              -> hash-bound receipt -> guarded commit -> required remote CI
```

Run it with `scripts/agent-harness`; task evidence is stored once under the
shared Git common directory at `.git/agent-harness/v2/tasks/<task-id>/`. Legacy
`.codex/tmp/` verdicts and trace helpers are historical evidence only and have no
authority over commits.

The finish guard is fail-closed. It accepts only a runner-created PASS bound to
the exact staged diff, active instructions, dependency revisions, green checks,
and independent review sessions. `scripts/agent-config-audit --ci` prevents the
Codex and Claude adapters from silently drifting.

Quick verification:

```bash
scripts/agent-harness doctor
scripts/agent-harness selftest --ci
scripts/agent-config-audit --ci
```

Local hooks are defense in depth, not a remote security boundary. Merge safety
ultimately requires GitHub to make the macOS `CI` check required on `murmur`.
Inspect that read-only with `scripts/agent-remote-audit`.
