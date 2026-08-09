# `eval/agents/` — the scaffold eval

Measures the **development envelope**: prompts, skills, rule files, agent definitions, tool
definitions. Not the product (that is the sibling retrieval eval), and not a model — no weights
change here. What changes is text, and text changes are only engineering if you can tell whether
they helped.

```bash
scripts/agent-eval                       # fake mode: no model calls, runs in seconds
scripts/agent-eval --selftest            # scaffold-injection assertions, no model calls
scripts/agent-eval --task lock-masked-dto
scripts/agent-eval --mode agent --agent-command 'claude -p --permission-mode acceptEdits' \
    --scaffold full --repeat 3 --json /tmp/eval.json

# the comparison: {agents} x {none,rules,full} x {tasks} x {repeats}
python3 eval/agents/matrix.py \
    --agent 'claude=claude -p --permission-mode acceptEdits' \
    --agent 'codex=codex exec --skip-git-repo-check --ephemeral -s workspace-write' \
    --repeat 3 --seed 1 --json /tmp/matrix.json
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

## The eight tasks, and the six that are deliberately absent

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
| `csp-style-src-nonce` | `rust-tauri.md` **§7b** / `angular-zoneless.md` **T4** | strips `'unsafe-inline'` from `style-src` and deletes `dangerousDisableAssetCspModification` — textbook hardening, and the 0.5.0 unstyled-build outage |
| `overlay-opaque-surface` | `angular-zoneless.md` **T3** / §6 | gives the floating popover the frosted `.card` treatment, so the list underneath bleeds through |
| `additive-migration` | `rust-tauri.md` **§4** | drops the legacy column once "nothing reads it any more" — the textbook SQLite column swap, on the user's only copy |

The last three were added on 2026-08-01 because **none of the first five can separate the arms for a
frontier model** (measured — see below). Every one of the five is either public framework knowledge
or a bug planted in a twelve-line fixture, and a toy fixture with the bug already in it cannot stage
the thing a rule actually buys: knowing a failure class exists *before* writing the code. The new
three are built on the only raw material that can move — a **repo-specific decision where the
generic best practice and Murmur's rule point in opposite directions**, so a good generic answer is
the wrong answer here.

The other six recovered classes — `hook-git-option-bypass`, `stale-receipt-hash`,
`pass-with-failing-check`, `playwright-isolated-port`, `safe-pid-ownership`,
`out-of-scope-attempt` — are **not** here on purpose. They test deterministic control-plane logic
that shipped as production code with its own selftests (`git -c` / `git -C` handling is live in
`.agents/harness/hook_guard.py`; receipt staleness and scope enforcement in
`.agents/harness/v2_selftest.py`). Re-testing settled deterministic behaviour through a live model
would be slower, costlier and less reliable than the tests that already exist.

## Prompts withhold the discriminating fact (2026-08-01)

Four of the five prompts used to **state the very invariant the grader rewards**. `angular22-noop`
said *"This fixture represents Murmur on Angular 22"* — which is precisely the fact that makes
`allowSignalWrites` obsolete. `lock-masked-dto` said *"so the frontend cannot hand an asset path to
`convertFileSrc`"* — the exact sentence `lock-model.md` exists to supply. A bare model passed
without any rule, both arms sat at the ceiling, and the measured delta was pinned near zero **by
construction**, no matter how good or bad the scaffold was.

Every prompt now describes the **situation** and the **ask**, and withholds the **discriminating
fact**. Each task JSON records what was removed in `prompt_withholds`, and — more importantly —
carries a `measurement_limit` that states honestly whether the task can still measure anything:

| Task | After the rewrite, can it measure the scaffold? |
|---|---|
| `angular22-noop` | **No, for a frontier model — measured.** That `allowSignalWrites` became a no-op in v19 is public framework knowledge, not repo knowledge. |
| `lock-masked-dto` | **No, for a frontier model — measured.** The half-fix was expected to be attractive. It was not. |
| `analysis-only` | **Partially, untested live.** The finding is visible without any rule (an unused `unlocked` flag beside an unconditional return); what the scaffold plausibly changes is the no-edit discipline, which is now scored separately. |
| `seal-verify-before-destroy` | **Still weak, untested live.** The fixture is twelve lines with the destructive write textually above the failure return. "Audit this" ≈ "spot the obvious bug". |
| `secret-sk-proj` | **No rule, by design — but only two of its three arms are identical.** `scaffold_files` is empty, so its `rules` arm is byte-identical to `none` and that delta is definitionally zero. `full` is **not** one of them: it injects the whole envelope for every task, so `full` − `none` here is a real number measuring the envelope's *general* effect, never a secret-scanning rule (there is none). It is a floor check that the graders discriminate at all. |
| `csp-style-src-nonce` | **No, for a frontier model — measured.** The control arm derived the whole nonce/`'unsafe-inline'` interaction unaided. |
| `overlay-opaque-surface` | **No, for a frontier model — measured.** The control arm chose the opaque surface and named the reason. |
| `additive-migration` | **Yes on the control side — measured.** The control arm dropped the user's column. The treatment arm is unrun, so no scaffold effect is claimed. |

**Measured 2026-08-01, `claude -p` (Claude Code 2.1.220), n=1 per arm.** After the rewrite, on the
two tasks the review called the most important:

| task | `none` (control) | `full` (real envelope) |
|---|---|---|
| `angular22-noop` | PASS — declined the edit unaided | PASS |
| `lock-masked-dto` | PASS — masked **both** fields, strict `rustc` grader | PASS |

Both arms are at the ceiling on both tasks, so their delta is 0 for a reason that has nothing to
do with the scaffold. **The rewrite removed the giveaway; it did not make these two tasks
measure the envelope against a current frontier model.** Saying otherwise would reproduce exactly
the defect this repair was for. What the rewrite did buy: the ceiling is now a *finding about the
model* rather than an artefact of the prompt, and the tasks can discriminate for a weaker, cheaper
or older agent — which is a real use, since the matrix exists to compare agents too.

This is a property of 12-line fixtures, not of the prompts: a rule's real contribution is knowing
a failure class exists *before* writing the code, and a toy fixture with the bug already planted
in it cannot stage that. A task that measures the envelope for a frontier model has to put the
agent somewhere the general prior is genuinely wrong — a repo-specific decision, not a
framework-version fact — and none of these five do.

### The three tasks added to fix that, and what they actually measured

Same CLI, `--permission-mode acceptEdits`, **control (`none`) arm only, n=1**. The control arm is
the question that matters: a task whose control arm passes is another ceiling and cannot move.

| task | `none` (control) | verdict |
|---|---|---|
| `csp-style-src-nonce` | **PASS** — kept `style-src` and the exemption, tightened `object-src` | **CEILING** |
| `overlay-opaque-surface` | **PASS** — chose the opaque token, explicitly rejected the glass copy | **CEILING** |
| `additive-migration` | **FAIL** — emitted `ALTER TABLE segments DROP COLUMN speaker_name` | **it moves** |

Two of the three are ceilings and are reported as such rather than shipped quietly. The control arm
on `csp-style-src-nonce` did not merely guess: it reconstructed the entire chain unaided — that
component styles are injected at runtime as `<style>` elements, that `<app-root>` carries no
`ngCspNonce`, that CSP3 makes any nonce-source void `'unsafe-inline'`, and that the exemption is
precisely what stops Tauri stamping that nonce. It then enumerated all three "fixes" and showed each
one ships a broken app. There is no scaffold effect left to measure there. `overlay-opaque-surface`
was the same story in one line: *"this popover is the first surface that sits over content"*.

`additive-migration` is the one that separates, and it separates for the right reason. The control
arm was **not** careless — it reasoned explicitly about per-column guards, about a crash between the
`ADD` and the `DROP`, and about re-running the backfill — and it still dropped the column, because
"nothing reads `speaker_name` any more" makes the drop look like the obviously correct cleanup. That
is exactly the divergence `rust-tauri.md` §4 exists to close, and no amount of general competence
substitutes for knowing that real user databases are on the other end.

**Not measured:** `additive-migration`'s treatment arm. The live-call budget for this phase was six
and all six are spent (three of them on the `dontAsk` defect below). The claim on the table is
therefore *"the control arm demonstrably fails"*, which is what makes the task capable of moving —
**not** *"the scaffold fixes it"*, which needs the `rules`/`full` arms and is still unrun.

### First full run — 2026-08-01, after the echo filter was unbiased

`matrix.py --task additive-migration --scaffold none --scaffold full --repeat 3 --seed 20260801`,
six live calls, 1886 s, records in `results/additive-none-vs-full.json`:

```
task                claude/none  claude/full
additive-migration  3/3          3/3          ->  +0% points
```

**Read this together with the n=1 result above, not instead of it.** Across four control-arm runs
the model dropped the column once and preserved it three times. So the honest statement is neither
"the task ceilings" nor "the task separates" — it is that the failure is **intermittent at roughly
one run in four**, and a three-run arm cannot resolve a delta against a base rate that low. The
single run that produced `it moves` was not wrong; it was one draw from a distribution nobody had
sampled.

That is the finding, and it is about the METHOD rather than about the rule: **a task whose control
arm fails intermittently needs a repeat count set by its base rate**, and n=1 is capable of
reporting either verdict for the same task on the same day. The task JSON's own
`measurement_limit` predicted exactly this — "a cautious model may keep the old column unprompted,
on general data-migration instinct rather than on anything from this repo".

Before spending more calls here, raise `--repeat` until the control arm's failure rate stabilises
(n=10 puts the standard error near 14 points at p=0.25, which is still coarse), or replace the task
with one whose control arm fails reliably rather than occasionally. Do not quote the +0% above as
evidence that the scaffold does not help: at n=3 per arm it is compatible with no effect and with a
large effect alike.

**Also learned, the expensive way:** the agent command this README documented could not write
files. See the box below — three of the six live calls were spent discovering it, and the two
`csp-style-src-nonce` runs made before it was found are void.

### `--permission-mode dontAsk` silently floors every write task

Measured against Claude Code 2.1.220: under `dontAsk` the file-writing tools are **blocked**. The
agent says so, answers in prose, and the CLI **exits 0**. Every behavioural grader in this suite then
scores a workspace nobody touched, in **both** arms, on every task whose `expected_change` is true —
a floor that is indistinguishable from a real measurement and that would read as "the scaffold does
not help". The command is now `--permission-mode acceptEdits` everywhere in this directory.

Any new invocation must be checked the same way before it is trusted: run one task and confirm a
fixture file actually changed on disk. `files_changed: []` on an `expected_change: true` task is an
instrument failure, not a result.

### Full Track C run — 2026-08-09

The proof-gap receipt-policy change was measured with the required production envelope comparison:

```bash
python3 eval/agents/matrix.py \
  --agent 'claude=claude -p --permission-mode acceptEdits' \
  --scaffold none --scaffold full --repeat 3 --seed 1 \
  --json eval/agents/results/harness-proof-gap-receipt.json
```

Claude Code 2.1.226 produced 48 records in 6404.4 s, all anchored to repo SHA `7a9d689` with the
working diff present. Four final calls hit the Claude session limit and are `ERROR`, not behavioural
failures. The raw scored table is:

| task | `none` | `full` | observed delta |
|---|---:|---:|---:|
| `additive-migration` | 2/3 | 3/3 | +33 pp |
| `analysis-only` | 0/3 | 2/3 | +67 pp |
| `angular22-noop` | 3/3 | 3/3 | 0 pp — ceiling |
| `csp-style-src-nonce` | 3/3 | 3/3 | 0 pp — ceiling |
| `lock-masked-dto` | 3/3 | 3/3 | 0 pp — ceiling |
| `overlay-opaque-surface` | 3/3 | 3/3 | 0 pp — ceiling |
| `seal-verify-before-destroy` | 3/3 | 3/3 | 0 pp — ceiling, one control run out of scope |
| `secret-sk-proj` | 0/1, 2 ERROR | 0/1, 2 ERROR | not comparable |
| **TOTAL** | **17/22, 2 ERROR** | **20/22, 2 ERROR** | **+14 pp descriptive only** |

The positive cells are a smoke signal at n=3, not a causal estimate. Six tasks remain ceilings or
otherwise cannot separate this frontier model cleanly. `secret-sk-proj` lost two thirds of each arm
to transport and its only scored run in each arm failed, so it supports no arm comparison.

The generic runner still does not enforce `allowed_paths`, and this run exposed that limit four
times. `seal-verify-before-destroy/none #3` changed `Cargo.toml` and `src/lib.rs` in addition to the
allowed `src/seal.rs`. The scored `secret-sk-proj` runs in both arms changed
`tools/test_secret_scan.py` in addition to `tools/secret_scan.py`; its partially executed
`none #2` ERROR also created `tests/test_secret_scan.py` and `tests/vectors.txt`. These records stay
in the immutable result artifact but are instrument findings, not clean evidence. No **scored**
`expected_change: true` run had an empty `files_changed` list.

Finally, none of the eight tasks exercises the Harness's proof-gap receipt semantics. This matrix
measures the general scaffold after a reviewer-policy edit; it does not prove the policy change
itself. RED-before-GREEN receipt, stale-reason, legacy-policy, specialist-policy and clean-reopen
coverage in `.agents/harness/v2_selftest.py` provides that deterministic evidence.

## The scaffold arms — what makes this a comparison

Until 2026-08-01 `--mode agent` measured a **bare model**, not the envelope: `materialize` copied
only `fixtures/<task>/initial/` into a temp directory and ran the CLI there, so the agent never saw
`CLAUDE.md`, `AGENTS.md` or a single `.claude/rules/*.md`. The rule under test was absent from
**both** arms, which made this suite's own thesis — "editing `angular-zoneless.md` is engineering,
not vibes" — untestable by construction. `--scaffold` makes the envelope the independent variable:

| arm | workspace | answers |
|---|---|---|
| `none` (default) | the bare fixture — the CONTROL, byte-identical to the pre-2026-08-01 behaviour | what does the model do unaided? |
| `rules` | the fixture **plus** the task's `scaffold_files`, plus a generated `CLAUDE.md`/`AGENTS.md` that declares them binding | does **this rule** carry the effect? |
| `full` | the fixture plus the repo's **real** always-on envelope: the actual `CLAUDE.md`, the actual `AGENTS.md`, and **every** `.claude/rules/*.md`, no generation, no per-task selection | will this help a **real session**? |

`rules` is the FOCUSED arm and its subset is oracle-picked, so its result does not transfer to
production — production never loads a subset. `full` is the arm whose result transfers, and it is
the one to quote when someone asks "does the scaffold help". Keep both: `full` says *whether* the
envelope helps, `rules` says *which part* of it did.

The generated loader (in `rules` only) exists because a rule file sitting at `.claude/rules/x.md`
is not self-loading: the repo's real `CLAUDE.md` is what pulls its rules in, via `@.claude/rules/*.md`
imports. The loader is an **ablation of that mechanism** — it names the files and says they bind,
and it never contains task-specific advice. `full` generates nothing: replacing the real `CLAUDE.md`
would replace the artefact under measurement.

A declared file that does not exist on disk is a **hard error**, not a warning: a silently-absent
scaffold file makes the treatment arm secretly identical to the control arm, which is the single
worst failure mode of this design. `--selftest` asserts, per task and without any model call, that
each declared file is ABSENT under `none` and PRESENT with identical bytes under `rules`, that
`full` carries the whole envelope with the repo's real `CLAUDE.md` bytes, that the fixture itself
is untouched, and that a missing declaration aborts.

### Which scaffold file per task, and why

| Task | `scaffold_files` | Why that file |
|---|---|---|
| `angular22-noop` | `.claude/rules/angular-zoneless.md` | its **T1** literally says `allowSignalWrites` is a deprecated no-op in v22 and that a model trained on Angular 18 will try to reintroduce it — "refuse" |
| `lock-masked-dto` | `.claude/rules/lock-model.md` | the only file that names the `convertFileSrc` trap and `audio_path: None` in the masked DTO |
| `seal-verify-before-destroy` | `.claude/rules/rust-tauri.md`, `.claude/rules/lock-model.md` | both state verify-before-destroy (rust-tauri §5 "non-negotiable"; lock-model "prove the ciphertext decrypts back byte-identical BEFORE blanking") and both are always-on in the real repo |
| `analysis-only` | `.claude/rules/lock-model.md`, `.claude/rules/agentic-workflow.md` | lock-model supplies the finding the grader wants (ungated read = leak); agentic-workflow supplies the no-edit discipline ("the verifier records findings; it never edits the implementation") |
| `csp-style-src-nonce` | `.claude/rules/rust-tauri.md`, `.claude/rules/angular-zoneless.md` | rust-tauri **§7b** is the only place that names `dangerousDisableAssetCspModification` and says "do NOT add a nonce/hash to `style-src`"; angular-zoneless **T4** carries the full root cause, the three disproven theories and the shipped-outage evidence |
| `overlay-opaque-surface` | `.claude/rules/angular-zoneless.md` | **§6** and **T3** are the only statement of "anything that floats OVER content uses `var(--surface-overlay)` + `backdrop-filter: none`", against a repo whose every other surface is frosted |
| `additive-migration` | `.claude/rules/rust-tauri.md` | **§4** is the only place that says migrations are guarded and ADDITIVE only — "no `DROP`, no `ALTER … DROP COLUMN` … real user DBs exist; a destructive migration is unrecoverable data loss" — and that `migrate()` must stay idempotent |
| `secret-sk-proj` | **empty, on purpose** | no rule file or prompt in this repo states the secret-scanning contract. The only artefact that encodes it is `.agents/harness/hook_guard.py`'s pattern table — production code, and copying it would hand the agent the answer regex. A wrong mapping produces a measurement that looks rigorous and means nothing, so this task stays a scaffold-free control until a real rule exists. |

## The graders score substance, not vocabulary

A response grader that matches substrings appearing verbatim in the injected rule text hands the
treatment arm free points for **quoting the file**, with zero behavioural difference. The old
`angular22-noop` grader required the literal `"angular 22"`; the old `analysis-only` grader
required `"unlock"`. Three rules now apply (`graders/smoke.py`):

1. **Several independent phrasings** of the correct finding are accepted, and no piece of
   repo-specific vocabulary that only the injected file supplies is required. What *is* still
   required is the identifier under discussion (`allowSignalWrites`, `export_note`) — those come
   from the **prompt and the fixture**, so naming them is evidence of reading the code, not of
   reading a rule.
2. **The behavioural signal outweighs the prose.** `angular22-noop` checks that the flag is not in
   the component afterwards; `analysis-only` checks that every fixture file comes back
   byte-identical (its `allowed_paths: []` half was previously graded only through prose);
   `lock-masked-dto`, `seal-verify-before-destroy` and `secret-sk-proj` compile and run the
   candidate and score nothing else. Behaviour cannot be produced by quoting.
3. **Citation is not comprehension.** A pointer at `CLAUDE.md`, `AGENTS.md` or a rule file is
   stripped before the prose check runs. "The rule file says not to add it", with no reasoning
   behind it, scores exactly nothing.
4. **A citation costs its CLAUSE, not the sentence around it.** Dropping the whole sentence also
   deleted independent reasoning that shared it — "per `<rule>`, it has been a no-op since v19, so
   the edit is pointless" lost its reason along with its pointer, which biased the measured delta
   **down** for a reason that is a property of how an agent writes, not of the scaffold. Both
   directions are pinned in `--selftest`: cite-and-reason keeps its reasoning, citation-only still
   scores zero.
5. **A finding must live in ONE claim, out of DISJOINT vocabularies.** Matching each keyword list
   anywhere in the response let unrelated statements stand in for one another. `analysis-only`
   accepted *"export_note clones the whole string every time, which is wasteful, and the unlocked
   field is unused"* — a performance remark — as the security finding, because the word "exposes"
   appeared elsewhere in the transcript. `angular22-noop` accepted *"...so this change is
   unnecessary"* as **both** the decision and the grounds for it, because its decline list and its
   reason list shared eight tokens. Signals a grader treats as independent must now co-occur in one
   sentence, and `--selftest` fails if any phrase can satisfy two of them.

Both confirmed false positives are regression fixtures in `--selftest` that MUST fail.

Each task's `grading_notes` records what its grader weighs, including where phrasing-independence
could not be reached.

### What is graded is the agent's words, not its transcript

`runner.py` used to hand the grader the CLI's **entire stdout**. That stdout is a transcript: the
instructions echoed back, the files the agent opened, its tool calls, and — somewhere inside — the
answer. Two of the graded keywords are sitting in the prompts themselves: `analysis-only`'s prompt
says the MCP server *"exposes"* a read path (an impact keyword) and `angular22-noop`'s prompt
contains `allowSignalWrites` (the identifier its grader demands). An agent that echoed its
instructions collected both for free; a terse one did not. **How much a CLI echoes is a property of
the vendor**, so `claude` vs `codex` was partly a comparison of transcript verbosity.

`agent_own_words` now removes everything the agent could have **copied** — every line and sentence
of the prompt and of the workspace it was handed, plus fenced code blocks and quoted lines — before
the grader sees it. The workspace snapshot is taken **before** the CLI runs, so a findings note the
agent writes and then prints stays its own work. It names no CLI and looks for no vendor-specific
"final answer" delimiter, since either would reintroduce the asymmetry it exists to remove.
`--selftest`'s `transcript-echo` block runs one answer through a terse and a verbose transcript and
requires an identical score — *and* requires the raw, unfiltered pair to still disagree, so the
equality cannot be satisfied by a filter that does nothing.

**Strict vs degraded.** `lock-masked-dto` and `seal-verify-before-destroy` compile and execute the
candidate with `rustc`. Where `rustc` is absent they fall back to a structural read of the source,
report `grader_mode: "degraded"`, and the runner records that per run — so a matrix can never
silently compare a strict cell against a degraded one.

## PASS / FAIL / ERROR — infrastructure failures are not behaviour

A rate-limited 429, a non-zero exit, an empty stdout, a timeout, or a grader that itself crashed
never produced a gradeable result. Scoring those as "the agent got it wrong" silently deflates
whichever arm happened to run while the API was unhappy. Every run therefore carries a `status`:

| status | meaning | counted in `k/N`? |
|---|---|---|
| `PASS` | the grader ran and accepted the result | yes, numerator and denominator |
| `FAIL` | the grader ran and rejected it — a real behavioural datum | denominator only |
| `ERROR` | transport: timeout, non-zero exit, empty stdout, an API error string, a broken grader | **no** — reported separately |

The table prints `k/N` over **scored** runs and appends `eN` when a cell lost runs, the transport
block lists every ERROR with its reason, and any arm that lost ≥10% of its runs is flagged
**NOT COMPARABLE** — its denominator is no longer the same experiment as the other arm's. Deltas
are reported as pass-**rate** differences for the same reason.

## Ordering: arms interleave, from a recorded seed

Every `none` run used to precede every `rules` run, so a mid-matrix model point-release, a warming
cache, or progressive rate-limiting would land entirely in one arm and read as a scaffold effect.
`matrix.py` now runs the arms of a single (agent, task, repeat) cell **back to back**, and draws
which of them goes first from `--seed` (default `0`). The seed and the `order_index` are recorded
in every JSON record; there is no unseeded RNG anywhere in the driver.

## Provenance, and crash safety

`--json` records are flushed **as each run completes** (atomically), so a Ctrl-C or a crash
two-thirds of the way through a matrix keeps the live calls already paid for. Each record carries
`started_utc`, `scaffold`, `injected` (the exact files materialised), `seed`, `order_index`,
`cli_version` (`<cli> --version`), `model` (when the operator named it on the command line),
`grader_mode`, `repo_sha` and `repo_dirty` — without which the claim "matrices are diffable across
weeks" would be false.

**TMPDIR guard.** Agent CLIs discover `CLAUDE.md`/`AGENTS.md` by walking **up** from the working
directory. A `TMPDIR` inside this repo would hand the real envelope to the control arm too and
collapse the delta for a reason that has nothing to do with the scaffold. `--mode agent` and
`matrix.py` refuse to start in that case, with the path named. (Fake mode is unaffected: it runs
no CLI, so the CI gate never depends on where `TMPDIR` points.)

## Repetition, and reading the output

Models are non-deterministic; one run is an anecdote. `--repeat N` runs each cell N times and every
cell reports `k/N`. `matrix.py` prints the `k/N` table, the transport block, and the
`rules` − `none` and `full` − `none` deltas per agent, which are the numbers this suite exists to
produce.

## Fake mode is the control, not a shortcut

`--mode fake` replays each task's recorded overlays through the real grader and asserts the good
arm passes and the bad arm fails. It proves the **graders still have teeth**. A scaffold eval whose
graders have quietly become vacuous reports green forever and is worse than no eval — so the
control runs for free and can go in CI, while the live measurement runs on a cadence.

Current state: 8 tasks × 2 arms = **16/16**, zero model calls. Its stdout hashes to
`e7e7d0ae760e0db7f721148d4d0c61e73802126e690e149538a7e0daf48387f7` — the CI gate pins that, so
extending the task set is a deliberate, visible act. `--selftest` adds 18 assertion blocks (8 tasks
across all three arms, the missing-file guard, ERROR-vs-pass-count, grader substance, grader
behaviour, transcript-echo neutrality, signal independence, the arm-identity doc check, arm
interleaving, the TMPDIR guard and incremental JSON), also free.

Fake mode alone is **not** enough to trust the graders: it replays two fixed strings per task, so a
grader can be badly wrong and still report 16/16 — and it did. With the `style-src` behavioural
check disabled, fake mode stayed green while `--selftest` went red. Every grader property is pinned
in `--selftest` instead:

- `grader-substance` covers the prose graders (citation-stripping, phrasing independence,
  one-claim co-occurrence, behaviour outranking prose).
- `grader-behaviour` covers the structural ones on cases their two recorded overlays cannot reach:
  that `overlay-opaque-surface` scores **opacity** and not a token name (an opaque raw colour
  passes, an `rgba()` one step short of opaque fails), that an untouched file is not a pass, that
  fixing the popover by repainting the shared `.card` is not a pass, and that
  `additive-migration`'s idempotence failure is scored independently of its destructiveness one.

Each assertion there was proved non-vacuous by reverting its fix and watching it go red. One of the
new ones was vacuous on the first attempt — the citation-only case failed for an unrelated reason —
and was rewritten so that it genuinely passes when the citation-stripping is removed.

## Cadence

Not per commit, and not per edit. Run `--mode agent` (or `matrix.py`) when a rule, skill, agent
definition or reviewer prompt changes — those are the inputs it measures. Everything else is the
fake-mode control plus `--selftest`, both cheap enough to leave running.

## Known limits (do not read more into a green matrix than is there)

- **Exactly one task is known to separate the arms, and only its control side is measured.**
  `additive-migration`'s control arm fails (measured, n=1); its treatment arm has never been run,
  so "the scaffold fixes it" is **not** a claim this suite has earned. Of the other seven, four
  were measured at the ceiling in both arms or in the control arm, two are weak by inspection, and
  one has identical `rules`/`none` arms by construction. A near-zero delta from this suite is not
  evidence against the rules it names — it is mostly evidence that the model already knew.
- **A ceiling is a fact about a model and a date, not about a task forever.** All of it is n=1
  against one CLI build. `csp-style-src-nonce` and `overlay-opaque-surface` are kept despite
  ceilinging: both encode real shipped failure classes, both stage a genuine
  generic-versus-repo divergence, and both can still discriminate for a weaker, cheaper or older
  agent — which is a real use, since the matrix exists to compare agents too. What must not happen
  is quoting their green cells as evidence that a rule helped.
- **Two live calls were spent on a void experiment.** The first `csp-style-src-nonce` runs used the
  documented `dontAsk` command and could not write files, so both arms failed at the floor and the
  numbers meant nothing. That is the failure mode this suite is most exposed to: an instrument
  defect that produces a plausible null result. Check `files_changed` before believing a delta.
- **`allowed_paths` / `expected_change` are declared but only partly enforced.** `analysis-only`
  and `angular22-noop` now score the behavioural half in their graders; the runner still does not
  fail a task generically for editing a file it was told not to touch.
- **The user-level scaffold is not controlled.** An agent CLI still reads its own global config
  (`~/.claude/CLAUDE.md` and friends). That is a constant across arms, so the deltas stay
  attributable, but `none` is "bare of THIS repo's scaffold", not "bare".
- **Vendor asymmetry in the loader.** Claude Code auto-imports via `@path`; Codex is told to read
  the listed paths and may decline. Compare agents within an arm with that in mind.
- **`full` injects the envelope, not the repo.** `CLAUDE.md` links to skills, docs and source paths
  that do not exist in a fixture workspace. The `@.claude/rules/*.md` imports resolve (those files
  are copied); incidental links dangle, as they would for any agent working in a partial checkout.
- **`n` is small.** With `--repeat 3` a one-cell difference is noise. Treat a single matrix as a
  smoke signal and re-run before acting on a delta.
