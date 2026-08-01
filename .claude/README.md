# `.claude/` — Murmur's Claude Code control surface

Claude Code is one adapter to the vendor-neutral development harness in
`.agents/harness/`; it is not a separate source of workflow truth.

| Path | Purpose |
| --- | --- |
| `../CLAUDE.md` | Autoloaded project charter and rule index |
| `settings.json` | Project permissions, sandbox policy, and hook wiring |
| `settings.local.json` | Untracked per-machine deltas — audited, and never a policy reversal |
| `rules/` | Binding Rust, Angular, lock, and agent-loop rules |
| `agents/` | Thin specialist role prompts |
| `skills/` | Claude-facing mirrors of shared executable runbooks |
| `hooks/` | Claude adapters for the canonical hook guard |
| `learnings/` | Curated recurring lessons and append-only run journals |

The active development loop is:

```text
task contract -> isolated worktree -> developer edit -> exact-diff plan
              -> deterministic checks -> fresh combined/risk reviews
              -> hash-bound receipt -> guarded commit -> required remote CI
```

Run it with `scripts/agent-harness`; task evidence is stored once under the
shared Git common directory at `.git/agent-harness/v2/tasks/<task-id>/`. Legacy
`.claude/tmp/` verdicts and trace helpers are historical evidence only and have
no authority over commits.

The finish guard is fail-closed. It accepts only a runner-created PASS bound to
the exact staged diff, active instructions, dependency revisions, green checks,
and independent review sessions. `scripts/agent-config-audit --ci` prevents the
Claude and Codex adapters from silently drifting.

## Local overrides (`settings.local.json`)

`settings.json` is the declared posture. `settings.local.json` is untracked and
git-ignored, so it is the one file that can change the EFFECTIVE runtime policy
while every parity check and fingerprint above still passes. The audit's
`_local_settings` section closes that gap: it reads the local file when present
and compares it against the tracked document. The file cannot exist on a CI
runner, so its absence is a pass and the check can never fail a remote job.

A local override **may widen** — extra `sandbox.filesystem.allowRead`/`allowWrite`
paths outside the repository, extra `sandbox.network.allowedDomains`, extra
`permissions.allow` entries the tracked policy does not deny. It **may not
reverse** what the repository declares:

- `sandbox.allowUnsandboxedCommands: true` while `settings.json` declares `false`;
- a `sandbox.filesystem.allowWrite` entry covering any `.git` directory;
- a `permissions.allow` entry that re-grants, or a `permissions.deny` list that
  drops, anything the tracked `permissions.deny` declares (the comparison is
  derived from `settings.json`, so it follows a legitimate policy change).

**Local convenience must never widen the declared posture.** The harness does
not borrow this file: `.agents/harness/runtime.py` provisions each review with
its own sandbox, inlining `autoAllowBashIfSandboxed: false` /
`allowUnsandboxedCommands: false` for Claude reviewers and the
`murmur_harness_reviewer` permission profile with `network.enabled = false` for
Codex ones. Loosening the project sandbox therefore buys nothing inside the
harness and only weakens ordinary sessions.

When a machine genuinely needs one of those reversals, record it rather than
hide it. A top-level `_ack_policy_overrides` array of `"<key>: <reason>"`
strings downgrades the named key from a failure to a warning:

```json
{
  "_ack_policy_overrides": [
    "sandbox.allowUnsandboxedCommands: release machine signs and notarizes"
  ]
}
```

The auditable keys are `sandbox.allowUnsandboxedCommands`,
`sandbox.filesystem.allowWrite`, and `permissions.deny`. An acknowledgement is a
recorded justification, not a mute button: a bare key, an empty reason, or an
unrecognized key is itself a failure, an acknowledgement downgrades only the key
it names, and the resulting `[WARN]` line keeps the reversal visible in every
audit run.

Quick verification:

```bash
scripts/agent-harness doctor
scripts/agent-harness selftest --ci
scripts/agent-config-audit --ci
```

Local hooks are defense in depth, not a remote security boundary. Merge safety
ultimately requires GitHub to make the macOS `CI` check required on `murmur`.
Inspect that read-only with `scripts/agent-remote-audit`.
