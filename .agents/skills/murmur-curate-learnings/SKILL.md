---
name: murmur-curate-learnings
description: Promote mature Murmur learnings journal entries into an agent's binding Recurring patterns. Use when the user asks to curate lessons, promote repeated learnings, or names an agent learnings file under `.codex/learnings/`.
---

# Murmur Curate Learnings

Curate the compounding-lessons loop for one agent. The expected input is an
agent name that maps to `.codex/learnings/<agent>.md`.

## Steps

1. Validate the requested agent exists under `.codex/learnings/`. If unknown, list valid agents and stop.
2. Read `.codex/learnings/<agent>.md` and scan `## Run journal` for entries with `Status: journal`.
3. Find lessons that recur: two or more journal entries pointing at the same failure mode, or one entry the operator confirms is load-bearing.
4. For each promotion, add one tight imperative bullet to `## Recurring patterns`. Match the existing terse style and keep the section to about 20 bullets; if full, merge or drop the weakest existing bullet and say which.
5. Mark each source journal entry `**Status:** distilled (<today>)`, using `date +%F`.
6. Do not invent lessons that are not in the journal.
7. Report which bullets you added or merged and which journal entries you marked distilled.
