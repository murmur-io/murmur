# The compounding-lessons loop — canonical tree: `.claude/learnings/`

Every bug this project shipped was caught by an *independent adversarial check*, and every
one produced a lesson that is otherwise scattered across AGENTS.md / CLAUDE.md, the rules' "traps",
memory notes, and PR bodies. This directory makes that loop **explicit, per-agent, and versioned in
the repo** so a lesson learned once is never re-paid.

## Canonical tree vs generated mirror

`.claude/learnings/` is **canonical**. `/learn`, `/curate-learnings`, the `murmur-learn` /
`murmur-curate-learnings` skills, and any extractor pass all write HERE.

`.codex/learnings/` is a **generated, byte-identical mirror** — never edit it by hand. A hand-edit
is destroyed by the next sync, and `scripts/agent-config-audit` fails on the drift in the meantime
(`Claude/Codex learnings drift: <file> — run scripts/agent-sync-learnings`). After any change to
the canonical tree:

```bash
scripts/agent-sync-learnings          # regenerate the mirror
scripts/agent-sync-learnings --check  # verify only; exit 1 lists every drifted file
```

One file per agent (`<agent-name>.md`), each with two tiers:

### `## Recurring patterns` — canonical, executable input
Short binding imperatives ("Guard async effect results with a newest-request token").
Keep it **≤ ~20 bullets**. This is not a vendor journal: every dispatched agent MUST read
`.claude/learnings/<agent-name>.md` when it exists (binding rule in CLAUDE.md), the parity of the
generated mirror is enforced by `scripts/agent-config-audit`, and that byte-identical mirror is
bound into the Harness protocol hash (`.agents/harness/runtime.py::instruction_paths`) — so a
lesson recorded here changes the hash every dispatch is verified against. A
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

1. **Read** — the developer and reviewer read the relevant canonical `## Recurring patterns`; the
   Harness protocol hash binds the complete tree through the generated `.codex/learnings/` mirror.
2. **Work** — the developer implements; the adversarial-verifier / lock-security-reviewer gate it.
3. **Extract** — after the gates settle, append a `## Run journal` entry citing the artifact that
   revealed it, then regenerate the mirror.
4. **Curate** — periodically, promote 2+ similar journal entries into `## Recurring patterns`,
   mark the sources `distilled`, then regenerate the mirror.

## Entrypoints

- **Record one journal entry** (operator-observed) — `/learn <agent>: <lesson>`
  (Claude, `.claude/commands/learn.md`) or the `murmur-learn` skill
  (Codex, `.agents/skills/murmur-learn/SKILL.md`).
- **Promote mature patterns** — `/curate-learnings <agent>`
  (Claude, `.claude/commands/curate-learnings.md`) or the `murmur-curate-learnings` skill
  (Codex, `.agents/skills/murmur-curate-learnings/SKILL.md`).
- **Regenerate the mirror** — `scripts/agent-sync-learnings`.

All of them write the canonical `.claude/learnings/` tree, whichever vendor is driving.

The seed `## Recurring patterns` were distilled from AGENTS.md / CLAUDE.md, the binding rules in
`.claude/rules/`, and the operator's memory notes — they encode the T1–T4 traps, the lock
invariants, the 2026-06-27 release incidents, and the 6+ shipped-and-caught failure modes.
