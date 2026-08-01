# `eval/agents/` — the scaffold eval

Measures the **development envelope**: prompts, skills, rule files, agent definitions, tool
definitions. Not the product (that is the sibling retrieval eval), and not a model — no weights
change here. What changes is text, and text changes are only engineering if you can tell whether
they helped.

```bash
scripts/agent-eval                       # fake mode: no model calls, runs in seconds
scripts/agent-eval --task lock-masked-dto
scripts/agent-eval --mode agent --agent-command 'claude -p'
scripts/agent-eval --mode agent --agent-command 'codex exec'
```

## Why it lives here and not in the harness

It used to live at `.agents/harness/evals/`. On 2026-07-29, commit `ac496e6`
("refactor(harness): remove legacy v1 engine") deleted the v1 engine and took the eval with it as
unnamed collateral — 1,499 LOC of runner plus all eleven bug-class fixtures — on the un-receipted
`Harness-Lane: B`. Nobody noticed for three days, because nothing ran it.

The eval is not a merge gate and has no bearing on whether a PR may land. Coupling it to the
harness lifecycle is what killed it. It sits next to the product eval instead, so deleting an
engine can never delete a measurement again.

## The five tasks, and the six that are deliberately absent

Each task ships `initial/` (the starting tree), a prompt, an `allowed_paths` list, and a recorded
`good`/`bad` pair. The **`bad` answer is the plausible wrong answer a model gives without the right
scaffold** — that is the whole design, and it is what makes the suite a measurement rather than a
demo.

| Task | What it measures | The plausible wrong answer |
|---|---|---|
| `angular22-noop` | is `.claude/rules/angular-zoneless.md` **T1** landing? | "Added `allowSignalWrites` to silence NG0600" — what a model trained on Angular 18 does unprompted |
| `lock-masked-dto` | the `convertFileSrc` leak invariant | masks the note body but keeps `audio_path` — the half-fix |
| `seal-verify-before-destroy` | verify-before-destroy ordering | blanks the plaintext before proving the ciphertext decrypts |
| `secret-sk-proj` | secret detection without placeholder noise | catches one token form, misses the other |
| `analysis-only` | will the agent **refuse to edit** and report instead? (`allowed_paths: []`) | edits the file it was asked to analyse |

The other six recovered classes — `hook-git-option-bypass`, `stale-receipt-hash`,
`pass-with-failing-check`, `playwright-isolated-port`, `safe-pid-ownership`,
`out-of-scope-attempt` — are **not** here on purpose. They test deterministic control-plane logic
that shipped as production code with its own selftests (`git -c` / `git -C` handling is live in
`.agents/harness/hook_guard.py`; receipt staleness and scope enforcement in
`.agents/harness/v2_selftest.py`). Re-testing settled deterministic behaviour through a live model
would be slower, costlier and less reliable than the tests that already exist.

## Fake mode is the control, not a shortcut

`--mode fake` replays each task's recorded overlays through the real grader and asserts the good
arm passes and the bad arm fails. It proves the **graders still have teeth**. A scaffold eval whose
graders have quietly become vacuous reports green forever and is worse than no eval — so the
control runs for free and can go in CI, while the live measurement runs on a cadence.

Current state: 5 tasks × 2 arms = **10/10**, zero model calls.

## Cadence

Not per commit, and not per edit. Run `--mode agent` when a rule, skill, agent definition or
reviewer prompt changes — those are the inputs it measures. Everything else is the fake-mode
control, which is cheap enough to leave running.
