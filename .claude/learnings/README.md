# `.claude/learnings/` — the compounding-lessons loop

Every bug this project shipped was caught by an *independent adversarial check*, and every
one produced a lesson that is currently scattered across CLAUDE.md, the rules' "traps", memory
notes, and PR bodies. This directory makes that loop **explicit, per-agent, and versioned in the
repo** so a lesson learned once is never re-paid.

One file per agent (`<agent-name>.md`), each with two tiers:

### `## Recurring patterns` — curated, injected into every dispatch
Short binding imperatives ("NEVER write a signal in an `effect()` without `allowSignalWrites`").
Keep it **≤ ~20 bullets** — this section is prepended to the agent's dispatch prompt as
*"Previous lessons (binding — do NOT repeat these)"*, so it spends prompt budget every run. A
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

1. **Read** — before dispatching an agent, a workflow/skill prepends that agent's
   `## Recurring patterns` to the prompt (see `.claude/skills/ship-feature`).
2. **Work** — the agent implements; the adversarial-verifier / lock-security-reviewer gate it.
3. **Extract** — after the gates settle, append a `## Run journal` entry (via `/learn`, or an
   extractor pass) citing the artifact that revealed it.
4. **Curate** — periodically, `/curate-learnings <agent>` promotes 2+ similar journal entries
   into `## Recurring patterns` and marks the sources `distilled`.

## Commands

- `/learn <agent>: <lesson>` — append one journal entry (operator-observed). See
  `.claude/commands/learn.md`.
- `/curate-learnings <agent>` — review the journal, promote mature patterns. See
  `.claude/commands/curate-learnings.md`.

The seed `## Recurring patterns` below were distilled from CLAUDE.md, `.claude/rules/`, and the
`~/.claude` memory notes — they encode the T1–T4 traps, the lock invariants, the 2026-06-27
release incidents, and the 6+ shipped-and-caught failure modes.
