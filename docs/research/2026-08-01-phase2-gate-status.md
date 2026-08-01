<!-- Written 2026-08-01 at the close of the agent-flow rebuild program. Its job is to record what landed and, more importantly, what is deliberately NOT being executed yet and on what condition. -->
# Phase 2 gate status — the 26,300-line deletion stays gated

`docs/research/2026-07-31-harness-simplification-plan.md` §5 lists a Phase 2 that deletes roughly
**26,300 lines** of harness surface. `2026-08-01-harness-plan-verification.md` reviewed that plan
adversarially and cut several of its unsupported claims. This note records the state of its
precondition after today's program.

**Phase 2 is NOT executed, and must not be, until its own precondition is met:** one real feature
measured end to end through the simplified flow. Not a docs change, not a control-plane change —
a feature, with its Rust and Angular gates, going from branch to merge under the loop as it now
stands. Everything below exists to make that measurement possible and honest; none of it is that
measurement.

## What landed today

| Change | PR | What it does |
|---|---|---|
| Harness simplification phase 1 | #535 | Scope derived from the exact diff (`--owned` becomes a tripwire); scaffold eval restored; learnings named as required reading at dispatch |
| Keep-going rule | #538 | Commit/push/PR are agent work; opening a PR is not a turn boundary |
| Reviewer authority | #541 | `combined` demoted to advisory on measured evidence; specialists stay blocking; demotion can never remove a plan's last gate |
| Scaffold-eval graders in CI | #540 | `--mode fake` joins `scripts/ci.sh` — a grader that loses its teeth now fails the build |
| Comparative scaffold eval | #542 | `--scaffold none/rules/full`, the agent × arm × task matrix, ERROR separated from FAIL, provenance, `files_changed` |
| Dead weight removed | #539 | The v1-era architecture page and a migration artifact deleted; deferred SDD findings archived |
| Learning loop reconnected | this program | `.claude/learnings/` canonical with an audited mirror; verify findings become journal candidates; recurring patterns reach reviewer prompts |
| Local-settings audit + outcome metrics | this program | A local settings file can no longer silently reverse the declared sandbox posture; `metrics` reports reviewer PASS-rate and cost per accepted task |
| Program driver | this note's PR | `scripts/agent-program` replaces the "next action within 60 seconds" sentence with something that runs, gates, stops on red, and resumes |

## What the measurement must show

When the next real feature ships through this loop, record:

1. **Task ids consumed.** The plan's own diagnosis was that one feature consumed eleven. One
   feature should now consume one branch; a new task id per attempt is the failure being watched
   for.
2. **Wall-clock from branch to merge**, and how much of it was CI. Every PR in this program cost
   roughly thirteen minutes of CI and, because the repo requires up-to-date branches with
   auto-merge disabled, each merge invalidated the next branch. That is a real, measured cost of
   many small PRs and it should inform how Phase 2 batches its deletions.
3. **Whether the harness was escalated to at all**, and if so whether the mechanical test
   (`risk_classification.{lock,egress,protocol}`) selected it — or whether somebody made a
   judgement call, which is the behaviour the mechanical test exists to prevent.
4. **What the advisory reviewer said, and whether ignoring it was right.** #541 demoted `combined`
   on a corpus where it produced one BLOCKER in 98 reviews. The demotion's own risk is an escape:
   a real defect now recorded as advisory and shipped. One feature is not enough to settle that,
   but it is enough to notice.
5. **`agent-harness metrics` before and after.** The numbers now exist; the point of Phase 2 is
   that the deletion should be visible in them rather than argued from.

## Why the gate holds

The plan's headline savings were measured against a state that no longer exists: the build caches
it counted were deleted today (roughly 190 GB of `runtime/checks/{tmp,cargo-target,clang-cache}`
across 53 tasks, plus 330 GB of Cargo targets) while the evidence stores — `events.jsonl`,
`attempts/`, `logs/` — were kept intact, which is what `metrics` reads. Any Phase 2 argument that
rests on disk reclamation must be re-measured against that, not against the July numbers.

More importantly, deleting 26,300 lines is exactly the kind of change whose cost shows up later,
in the class of defect nobody thought to keep a gate for. The precondition is not bureaucracy: it
is the difference between removing scaffolding because a building stands, and removing it because
the scaffolding is expensive.
