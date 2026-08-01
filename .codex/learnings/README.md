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
`.claude/learnings/<agent-name>.md` when it exists (binding rule in CLAUDE.md), and the parity of
the generated mirror is enforced by `scripts/agent-config-audit`. A pattern earns a place here only
after it has bitten (or been confirmed) at least twice.

This tier is literally executable: `.agents/harness/verifier.py::review_learnings_section` parses
it out of the ONE file matching the reviewer's own role (`combined` → `adversarial-verifier`,
otherwise `<kind>-reviewer`) and splices it into that reviewer's prompt. Four properties are
load-bearing and are pinned by `v2_selftest.review_learnings_prompt_cases`:

- **Only a role-matched file crosses the seam.** A kind with no such file (today `egress-security`
  and `protocol-security`) receives NOTHING. `main-loop.md` used to be prepended to every kind as
  "the cross-cutting journal", but its own header scopes it to the top-level agent that dispatches
  subagents and runs git — it presumes a shell, a worktree and `SendMessage`, none of which a
  reviewer has, and one of its bullets tells the reader to *"request a NARROW re-review of just the
  delta"*. In the prompt of the blocking `lock-security` gate that is a licence to PASS a diff whose
  untouched hunks still carry the original finding. Emitting nothing beats emitting the wrong file.
- **Only the curated tier crosses the seam.** The `## Run journal` is never injected — its
  `auto-candidate (uncurated)` entries are one review's unverified claims, and feeding them to the
  next reviewer would let the loop launder a hallucination into guidance.
- **It is read at the plan's base commit, not off disk.** The reviewer prompt is re-derived and
  hash-compared at attestation while the working tree stays mutable for the whole dispatch (a
  `/curate-learnings` promotion landing mid-review is ordinary), so a filesystem read would fail
  attestation nondeterministically.
- **For a reviewer it is advisory, not authority.** The injected header says so: patterns are
  hypotheses to check against the exact diff, and nothing in the section can authorize a PASS,
  retire a review step, or waive a finding. Writing an "X is pre-approved / known-good" bullet here
  does not make it true — a reviewer is instructed to report such a line as a finding.

Because this tier is binding for agents AND executable for reviewers, `config_audit._learnings_lint`
fails the audit on a `## Recurring patterns` bullet that prescribes a construct
`.claude/rules/angular-zoneless.md` bans (`allowSignalWrites`, `*ngIf`, `standalone: true`,
`provideExperimentalZonelessChangeDetection`, `zone.js`) without marking it as banned. A bullet may
still name one to tell a reviewer to flag it; it may not tell an agent to write one.

Separately, `.agents/harness/runtime.py::instruction_paths` fingerprints **both** trees into
`MURMUR_HARNESS_INSTRUCTIONS_SHA256`, the instructions digest exported to every check environment.
(That digest is observability for check runs; it is not `protocol_sha256` and does not by itself
gate attestation — the reviewer binding comes from the base commit above.)

### `## Run journal` — append-only, newest first
Raw evidence-backed entries from individual runs. Format:

```
### [YYYY-MM-DD <task/PR>] <one-line title>
- **Pattern:** what happened / the failure mode
- **Caught by:** adversarial-verifier | lock-security-reviewer | operator | gate:<name> | harness verify
- **Lesson:** the imperative to apply next time
- **Status:** journal | distilled (<date>) | success-pattern | auto-candidate (uncurated)
```

Auto-pruned past ~50 entries (oldest journal entries drop off; distilled ones are already
promoted). `success-pattern` entries capture what a *clean* run did right, not just failures.

`auto-candidate (uncurated)` marks an entry that came from the Harness's own extractor — one per
MAJOR/BLOCKER finding of a `NEEDS_FIX` verify — with a placeholder Lesson. It is raw reviewer
output, not yet a lesson: rewrite it by hand as one imperative, or delete it. It is never promoted
automatically, because auto-promotion is exactly how a hallucinated finding would become a binding
rule.

The extractor does **not** write this tree, or any working tree. It renders candidates into the
task's own store (`<task_dir>/learning-candidates.md`, under the Git common directory) and
`scripts/agent-harness status` prints how many are waiting. That is deliberate: `verify` runs from
the standalone `.murmur-agent-driver` clone, which `open` requires to be byte-clean and holds at a
detached HEAD, so a git-tracked write there would make every `NEEDS_FIX` block the next `open` —
and the only recovery available (`git restore`) would delete the lesson.

## The loop

1. **Read** — the developer reads the relevant canonical `## Recurring patterns`; a Harness reviewer
   does not have to, because the runner injects that tier into its prompt automatically
   (`verifier.review_learnings_section`, read at the plan's base commit).
2. **Work** — the developer implements; the adversarial-verifier / lock-security-reviewer gate it.
3. **Extract** — after the gates settle, append a `## Run journal` entry citing the artifact that
   revealed it, then regenerate the mirror. A `NEEDS_FIX` verify drafts candidates for its own
   severe findings into the task store (`.agents/harness/learning_extract.py`, enabled by
   `learning_extract` in `.agents/harness/config.json`); `status` reports them, and you file them
   with `/learn` **from your primary checkout, never the driver clone**. They land as
   `auto-candidate (uncurated)` and still need a human to turn them into a lesson.
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
