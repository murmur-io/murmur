---
description: Append a lesson to an agent's learnings journal (.claude/learnings/<agent>.md).
argument-hint: "<agent>: <lesson>   e.g. rust-tauri-dev: gate list_*_visible reads or MCP leaks"
allowed-tools: ["Read", "Edit", "Bash"]
---

Record a learning in the compounding-lessons loop (see `.claude/learnings/README.md`).
`.claude/learnings/` is the canonical tree for both vendors; `.codex/learnings/` is a generated
byte mirror — write canonical, then regenerate the mirror.

Input: `$ARGUMENTS` — of the form `<agent>: <lesson text>`.

Steps:
1. Parse `<agent>` (before the first `:`) and `<lesson>` (after). Valid agents are the files in
   `.claude/learnings/` (angular-zoneless-dev, rust-tauri-dev, adversarial-verifier,
   lock-security-reviewer, release-engineer, murmur-researcher). If the agent is unknown, list the
   valid ones and stop.
2. Open `.claude/learnings/<agent>.md`. Insert a NEW entry at the TOP of the `## Run journal`
   section (newest first), using the exact template:
   ```
   ### [<today> operator] <one-line title derived from the lesson>
   - **Pattern:** <what happens / the failure mode, in the operator's words>
   - **Caught by:** operator
   - **Lesson:** <the imperative to apply next time>
   - **Status:** journal
   ```
   Use today's date from `date +%F`.
3. Do NOT touch `## Recurring patterns` — promotion is `/curate-learnings`'s job.
4. If the journal now exceeds ~50 entries, drop the oldest `journal`-status entries (never drop
   `distilled`/`success-pattern`).
5. Run `scripts/agent-sync-learnings` to regenerate the `.codex/learnings/` mirror — skipping it
   leaves `scripts/agent-config-audit` red.
6. Confirm the one-line title you added; do not restate the whole file.
