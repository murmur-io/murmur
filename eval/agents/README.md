# `eval/agents/` — the scaffold eval

Measures the **development envelope**: prompts, skills, rule files, agent definitions, tool
definitions. Not the product (that is the sibling retrieval eval), and not a model — no weights
change here. What changes is text, and text changes are only engineering if you can tell whether
they helped.

```bash
scripts/agent-eval                       # fake mode: no model calls, runs in seconds
scripts/agent-eval --selftest            # scaffold-injection assertions, no model calls
scripts/agent-eval --task lock-masked-dto
scripts/agent-eval --mode agent --agent-command 'claude -p --permission-mode dontAsk' \
    --scaffold rules --repeat 3 --json /tmp/eval.json

# the comparison: {agents} x {none,rules} x {tasks} x {repeats}
python3 eval/agents/matrix.py \
    --agent 'claude=claude -p --permission-mode dontAsk' \
    --agent 'codex=codex exec --skip-git-repo-check --ephemeral -s workspace-write' \
    --repeat 3 --json /tmp/matrix.json
python3 eval/agents/matrix.py --agent 'claude=claude -p' --dry-run   # plan it before paying for it
```

Both free checks (`--mode fake` and `--selftest`) belong in CI; neither makes a model call.

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

## The scaffold arms — what makes this a comparison

Until 2026-08-01 `--mode agent` measured a **bare model**, not the envelope: `materialize` copied
only `fixtures/<task>/initial/` into a temp directory and ran the CLI there, so the agent never saw
`CLAUDE.md`, `AGENTS.md` or a single `.claude/rules/*.md`. The rule under test was absent from
**both** arms, which made this suite's own thesis — "editing `angular-zoneless.md` is engineering,
not vibes" — untestable by construction. `--scaffold` makes the envelope the independent variable:

| arm | workspace |
|---|---|
| `none` (default) | the bare fixture — the CONTROL, byte-identical to the pre-2026-08-01 behaviour |
| `rules` | the fixture **plus** the task's `scaffold_files`, copied from the repo root at their real repo-relative paths, plus a generated `CLAUDE.md`/`AGENTS.md` that declares them binding |

The generated loader exists because a rule file sitting at `.claude/rules/x.md` is not
self-loading: the repo's real `CLAUDE.md` is what pulls its rules in, via `@.claude/rules/*.md`
imports. The loader is an **ablation of that mechanism** — it names the files and says they bind,
and it never contains task-specific advice. Claude Code follows the `@` import; Codex reads
`AGENTS.md` verbatim and opens the listed paths itself.

A declared file that does not exist on disk is a **hard error**, not a warning: a silently-absent
scaffold file makes the treatment arm secretly identical to the control arm, which is the single
worst failure mode of this design. `--selftest` asserts, per task and without any model call, that
each declared file is ABSENT under `none` and PRESENT with identical bytes under `rules`, that the
fixture itself is untouched, and that a missing declaration aborts.

### Which scaffold file per task, and why

| Task | `scaffold_files` | Why that file |
|---|---|---|
| `angular22-noop` | `.claude/rules/angular-zoneless.md` | its **T1** literally says `allowSignalWrites` is a deprecated no-op in v22 and that a model trained on Angular 18 will try to reintroduce it — "refuse" |
| `lock-masked-dto` | `.claude/rules/lock-model.md` | the only file that names the `convertFileSrc` trap and `audio_path: None` in the masked DTO |
| `seal-verify-before-destroy` | `.claude/rules/rust-tauri.md`, `.claude/rules/lock-model.md` | both state verify-before-destroy (rust-tauri §5 "non-negotiable"; lock-model "prove the ciphertext decrypts back byte-identical BEFORE blanking") and both are always-on in the real repo |
| `analysis-only` | `.claude/rules/lock-model.md`, `.claude/rules/agentic-workflow.md` | lock-model supplies the finding the grader wants (ungated read = leak); agentic-workflow supplies the no-edit discipline ("the verifier records findings; it never edits the implementation") |
| `secret-sk-proj` | **empty, on purpose** | no rule file or prompt in this repo states the secret-scanning contract. The only artefact that encodes it is `.agents/harness/hook_guard.py`'s pattern table — production code, and copying it would hand the agent the answer regex. A wrong mapping produces a measurement that looks rigorous and means nothing, so this task stays a scaffold-free control until a real rule exists. |

## Repetition, and reading the output

Models are non-deterministic; one run is an anecdote. `--repeat N` runs each cell N times and every
cell reports `k/N`. `--json <path>` writes one record per `(task, agent label, scaffold, run)` with
the verdict, the grader message and wall-clock seconds, so a matrix can be diffed across weeks.
`matrix.py` prints the `k/N` table plus the `rules` − `none` delta per agent, which is the number
this suite exists to produce.

## Fake mode is the control, not a shortcut

`--mode fake` replays each task's recorded overlays through the real grader and asserts the good
arm passes and the bad arm fails. It proves the **graders still have teeth**. A scaffold eval whose
graders have quietly become vacuous reports green forever and is worse than no eval — so the
control runs for free and can go in CI, while the live measurement runs on a cadence.

Current state: 5 tasks × 2 arms = **10/10**, zero model calls. `--selftest` adds 6 further
assertions (5 tasks + the missing-file guard), also free.

## Cadence

Not per commit, and not per edit. Run `--mode agent` (or `matrix.py`) when a rule, skill, agent
definition or reviewer prompt changes — those are the inputs it measures. Everything else is the
fake-mode control plus `--selftest`, both cheap enough to leave running.

## Known limits (do not read more into a green matrix than is there)

- **`allowed_paths` / `expected_change` are declared but not enforced.** Nothing in the runner
  fails a task for editing a file it was told not to touch; `analysis-only` is graded purely on
  its response text, so "will the agent REFUSE to edit" is currently measured only indirectly.
- **The user-level scaffold is not controlled.** An agent CLI still reads its own global config
  (`~/.claude/CLAUDE.md` and friends). That is a constant across both arms, so the `rules` − `none`
  delta stays attributable, but the `none` arm is "bare of THIS repo's scaffold", not "bare".
- **Vendor asymmetry in the loader.** Claude Code auto-imports via `@path`; Codex is told to read
  the listed paths and may decline. Compare agents within an arm with that in mind.
