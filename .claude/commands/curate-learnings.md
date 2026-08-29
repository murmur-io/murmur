---
description: Promote mature journal entries into an agent's binding Recurring patterns.
argument-hint: "<agent>   e.g. angular-zoneless-dev"
allowed-tools: ["Read", "Edit", "Bash"]
---

Curate the compounding-lessons loop for one agent (see `.claude/learnings/README.md`).
`.claude/learnings/` is the canonical tree for both vendors; `.codex/learnings/` is a generated
byte mirror — promote into canonical, then regenerate the mirror.

Input: `$ARGUMENTS` — the agent name (a file in `.claude/learnings/`). If unknown, list valid ones
and stop.

Steps:
1. Read `.claude/learnings/<agent>.md`. Scan `## Run journal` for entries with `Status: journal`.
2. Find lessons that recur — **2+ journal entries pointing at the same failure mode**, or a single
   entry the operator confirms is load-bearing. Only these are promotion-worthy; a one-off curiosity
   stays in the journal.
3. For each promotion:
   - Add ONE tight imperative bullet to `## Recurring patterns` (match the existing terse style;
     link related items with `[[…]]` where useful). Keep the section **≤ ~20 bullets** — if it is
     full, merge or drop the weakest existing bullet and say which.
   - Mark each source journal entry `**Status:** distilled (<today>)` (date from `date +%F`).
4. Do NOT invent lessons that aren't in the journal. Curation promotes evidence, it doesn't author.
5. Run `.agents/h/mirror-check --fix` to regenerate the `.codex/learnings/` mirror — skipping it
   leaves `.agents/h/mirror-check` red.
6. Report: which bullets you added/merged and which journal entries you marked distilled — concise,
   not a full file dump.
