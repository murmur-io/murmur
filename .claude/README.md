# `.claude/` — Murmur's Claude Code control surface

Claude Code is one adapter to the vendor-neutral development harness in
`.agents/h/`; it is not a separate source of workflow truth.

| Path | Purpose |
| --- | --- |
| `../CLAUDE.md` | Autoloaded project charter and rule index |
| `settings.json` | Project permissions, sandbox policy, and hook wiring |
| `settings.local.json` | Untracked per-machine deltas — audited, and never a policy reversal |
| `rules/` | Binding Rust, Angular, lock, and agent-loop rules |
| `agents/` | Thin specialist role prompts |
| `skills/` | Claude-facing mirrors of shared executable runbooks |
| `hooks/` | Claude adapters for the canonical hook guard |
| `learnings/` | **Canonical** curated recurring lessons and append-only run journals (`.codex/learnings/` mirrors it) |

The active development loop is:

```text
task contract -> isolated worktree -> developer edit -> exact-diff plan
              -> deterministic checks -> fresh combined/risk reviews
              -> hash-bound receipt -> guarded commit -> required remote CI
```

Run it with `scripts/h`; task evidence is stored once under the
shared Git common directory at `.git/h/<task-id>.json`. Legacy
`.claude/tmp/` verdicts and trace helpers are historical evidence only and have
no authority over commits.

The finish guard is fail-closed. It accepts only a runner-created PASS bound to
the exact staged diff, active instructions, dependency revisions, green checks,
and independent review sessions. `.agents/h/mirror-check` prevents the
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
`permissions.allow` entries the tracked policy does not deny, extra
`permissions.deny` entries of its own (deny lists union across sources, so a
local deny can only ever tighten). It **may not reverse** what the repository
declares:

- any tracked `sandbox` scalar flipped to its permissive side —
  `enabled`/`failIfUnavailable` `true → false`, `allowUnsandboxedCommands`
  `false → true`. Direction is per key: `autoAllowBashIfSandboxed: false` only
  adds prompts and is allowed. The audit fails if `settings.json` ever grows a
  sandbox scalar whose direction is not modelled, so the rule cannot rot;
- a `permissions.defaultMode` at or above the tracked one on the
  `plan < default < acceptEdits < bypassPermissions` ladder;
- a `sandbox.filesystem.allowWrite` entry covering any `.git` directory **or any
  ancestor of the repository root** (a write grant is a subtree grant, so `/`,
  `$HOME`, and the repo directory itself all reach `<repo>/.git/hooks`);
- a redeclared `sandbox.filesystem.denyWrite` that drops a tracked entry — the
  array replaces rather than merges, so omitting one removes it;
- a `permissions.allow` entry that re-grants anything the tracked
  `permissions.deny` protects, matched on the deny rule's literal path prefix
  rather than on the exact string, so `Read(~/.ssh/*)` and
  `Bash(cat ~/.ssh/id_rsa)` are caught as well as `Read(~/.ssh/**)`;
- an `env` value that differs from the one `settings.json` declares — notably
  `MURMUR_FINISH_GUARD` (usuniete wraz z receiptami; historyczne)
  it reads `off`.

Every one of those comparisons is derived from `settings.json`, so a legitimate
policy change moves the rule with it instead of leaving a stale second copy.

**Local convenience must never widen the declared posture.** The harness does
not borrow this file: `.agents/h/h.py` provisions each review with
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

The auditable keys are `env`, `permissions.allow`, `permissions.defaultMode`,
`sandbox.allowUnsandboxedCommands`, `sandbox.enabled`,
`sandbox.failIfUnavailable`, `sandbox.filesystem.allowWrite`, and
`sandbox.filesystem.denyWrite` — one key per reversal, so acknowledging a
sandbox scalar cannot also silence a credential re-grant. An acknowledgement is
a recorded justification, not a mute button: a bare key, an empty reason, or an
unrecognized key is itself a failure, an acknowledgement downgrades only the key
it names, and the resulting `[WARN]` line keeps the reversal visible in every
audit run.

Quick verification:

```bash
.agents/h/mirror-check
```

Local hooks are defense in depth, not a remote security boundary. Merge safety
ultimately requires GitHub to make the macOS `CI` check required on `murmur`.
Inspect that read-only with `scripts/agent-remote-audit`.
