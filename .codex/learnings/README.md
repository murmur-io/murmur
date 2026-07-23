# `.codex/learnings/` — the compounding-lessons loop

Every bug this project shipped was caught by an *independent adversarial check*, and every
one produced a lesson that is currently scattered across AGENTS.md, the rules' "traps", memory
notes, and PR bodies. This directory makes that loop **explicit, per-agent, and versioned in the
repo** so a lesson learned once is never re-paid.

One file per agent (`<agent-name>.md`), each with two tiers:

### `## Recurring patterns` — curated, injected into every harness dispatch
Short binding imperatives ("Guard async effect results with a newest-request token").
Keep it **≤ ~20 bullets** — the harness prepends a bounded, role-relevant selection to each
writer/reviewer prompt, so it spends prompt budget every run. A
pattern earns a place here only after it has bitten (or been confirmed) at least twice.

### `## Run journal` — append-only, newest first
Raw evidence-backed entries from individual runs. Format:

```
### [YYYY-MM-DD <task/PR>] <one-line title>
- **Pattern:** what happened / the failure mode
- **Caught by:** adversarial-verifier | lock-security-reviewer | operator | gate:<name>
- **Lesson:** the imperative to apply next time
- **Status:** journal | distilled (<date>) | success-pattern
```

Auto-pruned past ~50 entries (oldest journal entries drop off; distilled ones are already
promoted). `success-pattern` entries capture what a *clean* run did right, not just failures.

## The loop

1. **Read** — `task_runner.py::learning_prompt` prepends role-relevant canonical
   `## Recurring patterns`; `instructions_sha256` also binds the complete `.codex/learnings/` tree.
2. **Work** — the agent implements; the adversarial-verifier / lock-security-reviewer gate it.
3. **Extract** — after the gates settle, append a `## Run journal` entry with the
   `murmur-learn` skill, citing the artifact that revealed it.
4. **Curate** — periodically, `murmur-curate-learnings <agent>` promotes 2+ similar journal entries
   into `## Recurring patterns` and marks the sources `distilled`.

## Skills

- `murmur-learn` — append one journal entry (operator-observed). See
  `.agents/skills/murmur-learn/SKILL.md`.
- `murmur-curate-learnings` — review the journal, promote mature patterns. See
  `.agents/skills/murmur-curate-learnings/SKILL.md`.

The seed `## Recurring patterns` below were distilled from AGENTS.md, `.codex/rules/`, and the
`~/.codex` memory notes — they encode the T1–T4 traps, the lock invariants, the 2026-06-27
release incidents, and the 6+ shipped-and-caught failure modes.
