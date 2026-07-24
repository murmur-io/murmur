<!-- Generated 2026-07-24 via /research (murmur-researcher fan-out). Pricing/funding/version = point-in-time. -->
# Research: Faster harness lanes without weakening Murmur's truth boundary

## TL;DR / Verdict

Do not add a bypass that makes an unreviewed change look verified. Instead, make the existing
workflow explicitly two-speed:

`Candidate` (fast, local, non-merge-authoritative) -> `Verified` (exact-diff attestation plus
independent reviews) -> `Merge-ready` (Verified plus the required full GitHub CI) ->
`Release-ready` (Merge-ready plus signed-Mac build, notarization and stapling).

The two highest-leverage changes are:

1. Fan out immutable, read-only reviews in parallel after checks finish.
2. Add a strictly semantic `release-version` profile for the four version/pin files, rather than
   treating every `tauri.conf.json` edit as a runtime/boot change.

This speeds normal work and removes a demonstrated release-bump failure mode without weakening
the PR CI, lock/egress/protocol requirements, signing, notarization, or the exact-diff receipt.

## What exists today

- The runner owns `writer -> checks -> final checks -> spec/adversarial/risk reviews -> repair ->
  hash-bound PASS`; later edits invalidate the receipt. [`.agents/harness/README.md`](../../.agents/harness/README.md:5)
- The default is two repair rounds, two required reviews, specialist reviews for lock/egress/protocol,
  and path-derived canonical evidence. [`.agents/harness/config.json`](../../.agents/harness/config.json:3)
- The current runner invokes deterministic checks and reviewers serially, although reviewers receive
  the same immutable diff and completed evidence. [`.agents/harness/task_runner.py`](../../.agents/harness/task_runner.py:2829)
- CI is already the remote truth: one required, strict-up-to-date macOS full `ci.sh` gate, including
  audio E2E. [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml:40)
- The release pipeline is necessarily sequential after merge: universal build, inside-out signing,
  notarization, stapling and `spctl` verification. [`.agents/skills/release-murmur/SKILL.md`](../../.agents/skills/release-murmur/SKILL.md:144)

## Findings

### 1. Separate fast feedback from merge authority

GitHub's branch-protection model supports this distinction: required, fresh checks determine merge
eligibility; local work can be useful without being merge proof. [GitHub protected branches](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches)

| State | Evidence | May do | Must not do |
|---|---|---|---|
| `Candidate` | Exact diff, conservative risk classification, selected local checks | Iterate, show failures, request review | Claim PASS, commit through harness, merge |
| `Verified` | Exact-diff receipt, all canonical risk evidence, independent reviews | Harness commit and open PR | Remain valid after any diff/base/instruction change |
| `Merge-ready` | Verified + fresh full PR CI | Merge through the protected branch | Replace full CI with selected checks |
| `Release-ready` | Merge-ready + universal signed/notarized/stapled artifact evidence | Publish DMG | Infer notarization from CI |

An unavailable independent vendor should yield `AWAITING_REVIEW` / `BLOCKED`, never a fake
same-vendor PASS. Qualified-review guidance also favours alternate reviewers or queueing work rather
than manufacturing approval. [Google review speed](https://google.github.io/eng-practices/review/reviewer/speed.html), [Google reviewer qualifications](https://google.github.io/eng-practices/review/reviewer/looking-for.html)

### 2. Parallelize reviews, not the Rust resource lane

Spec, adversarial and specialist reviewers are read-only consumers of the same staged diff; dispatch
them concurrently, collect every result, then preserve the present requirement that *all* required
reviews PASS and the diff fingerprint remains unchanged. This shortens wall-clock time without
changing what is proved.

Do not parallelize Cargo/full CI locally. The global resource supervisor deliberately serializes the
always-compiled ML tree to prevent the Mac from locking up. [`.codex/rules/agentic-workflow.md`](../../.codex/rules/agentic-workflow.md:20)

### 3. Fix over-broad release version classification

Today any edit to `src-tauri/tauri.conf.json` is runtime-risk classified, injecting a boot smoke.
That is correct for bundle/startup changes but too broad for a version-only bump. [`.agents/harness/config.json`](../../.agents/harness/config.json:86)

The retained `release-1-0-2-version-bump` task demonstrates the cost: a correct metadata change was
blocked because another process occupied the smoke port. [task evidence](../../.git/agent-harness/tasks/release-1-0-2-version-bump/state.json:2)

Add `release-version` only if a strict validator proves that the diff contains exactly:

- `package.json` version;
- `src-tauri/tauri.conf.json` version only, with `com.meetnotes.app` unchanged;
- `src-tauri/Cargo.toml` version;
- root `Cargo.lock` pin for `murmur`.

It should retain independent review and the PR's full CI, but omit `tauri-boot`. Any extra line,
file, dependency/configuration change, unknown path or classifier uncertainty falls back to the
normal full profile.

### 4. Keep remote CI and release truth unchanged

Selective testing is valid for a draft/inner loop only when unknown changes conservatively fall back
to full coverage. [Microsoft Test Impact Analysis](https://learn.microsoft.com/en-us/azure/devops/pipelines/test/test-impact-analysis?view=azure-devops)

Keep current ref-scoped cancellation for superseded PR checks, but serialize and do not cancel
release signing/notarization work. [GitHub Actions concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)

Do not add a merge queue now: it addresses high concurrent merge volume and would require a
`merge_group` CI integration, while Murmur currently has one operator and one heavy macOS lane.
[GitHub merge queue](https://docs.github.com/en/enterprise-cloud%40latest/repositories/configuring-pull-request-merges/managing-a-merge-queue)

## Recommended implementation order

1. **Telemetry first (small):** record queue wait, writer/check/reviewer durations, profile/risk
   selection and terminal block reason in task evidence. This makes optimisation evidence-driven.
2. **Parallel reviewer fan-out (small):** deterministic selftests for concurrent dispatch, one
   failing review blocking PASS, and exact-diff revalidation after all results arrive.
3. **`release-version` profile (small/medium):** strict semantic diff validator plus RED tests that
   show a non-version `tauri.conf.json` edit still requires runtime evidence.
4. **Candidate UX (medium):** an explicit `candidate` command/state for focused local work. It may
   run selected checks and retain evidence, but can never call `commit`; promotion runs the normal
   verification flow on the same exact diff.
5. **Measure before more:** use the new durations for a week. Defer remote path-conditional CI,
   merge queues and release artifact attestations.

## Non-negotiable boundaries

- Lock, egress, protocol, recording/audio, performance, runtime and control-plane paths always
  retain their specialist/full requirements.
- Unknown paths fail closed to the normal profile.
- Full GitHub `ci.sh` stays required for every PR merge.
- Signing, notarization, stapling and `spctl` stay release-only hard gates on a real Mac.
- Candidate/degraded review labels never appear as `PASS` or release proof.

## Sources

- [GitHub protected branches](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches)
- [GitHub Actions concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)
- [GitHub merge queues](https://docs.github.com/en/enterprise-cloud%40latest/repositories/configuring-pull-request-merges/managing-a-merge-queue)
- [Microsoft Test Impact Analysis](https://learn.microsoft.com/en-us/azure/devops/pipelines/test/test-impact-analysis?view=azure-devops)
- [Google reviewer speed](https://google.github.io/eng-practices/review/reviewer/speed.html)
- [Google reviewer qualifications](https://google.github.io/eng-practices/review/reviewer/looking-for.html)
