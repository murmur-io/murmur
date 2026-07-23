# Loop Engineering benchmark: validation and hardening decision

Date: 2026-07-22
Scope: Murmur's **development-agent** control plane (`.agents/`, `.codex/`, `.claude/`), not the AI inside the product.

## Executive summary

The supplied 8.5/10 benchmark is directionally strong, but it mixes three categories:

1. real high-impact gaps (`fake` receipts, same-vendor review, no-progress handling, remote enforcement, performance governance);
2. useful medium-term program work (incident-derived evals, adapter-confinement canaries, broader verifier mutation testing);
3. recommendations that should **not** be copied literally (one stochastic model trial as a blocking PR gate, or noisy RSS/wall-time thresholds on a 7 GB hosted macOS VM).

The code audit also found a more serious gap not called out in the benchmark: risk evidence was bound only by a caller-chosen check ID. A task could declare `lock-negative::true` and satisfy the label-level requirement. This is now replaced by runner-owned, byte-exact canonical commands.

The recommended operating model is:

- deterministic workflow and graders own `PASS`;
- Claude writes and Codex reviews by default, with the reverse permitted but same-vendor review forbidden;
- bounded repair, task-wide deadline, and a deterministic no-progress circuit breaker;
- deterministic memory/lifecycle invariants block hot-path changes;
- signed-Mac footprint/Metal/TCC evidence remains a separate controlled lane;
- live model capability evals remain manual/scheduled until an individual task is stable enough to graduate;
- GitHub server-side protection is the remaining authoritative-boundary blocker and requires operator action.

## Method

Three independent research passes were synthesized:

- direct code/TCB audit of `task_runner.py`, `hook_guard.py`, schemas, evals, and live receipts;
- primary-source review of bounded autonomy, correlated model errors, eval validity, provenance, and false-success measurement;
- performance/governance review against the active recording-memory branch, hosted-runner constraints, Criterion guidance, and the live GitHub configuration.

No product vault, database, audio, transcript, Keychain item, model credential, or GitHub setting was mutated.

## Adjudication of the benchmark

### Confirmed and high priority

- **Production-accessible fake adapter:** confirmed. Public `init`, schemas, verifier, hook, and commit accepted synthetic writer/reviewer evidence.
- **Same-vendor review:** confirmed. Fresh sessions were enforced, different vendors were not.
- **No-progress detection:** confirmed. Per-process timeouts and repair-count bounds existed, but identical failed rounds still consumed the full loop.
- **Remote authority gap:** confirmed. The live branch has no required app-bound gate, strict status policy, approval, admin enforcement, conversation resolution, secret scanning, or push protection.
- **Performance gap:** confirmed. Correctness gates existed, but hot-path tasks had no automatically required deterministic memory/lifecycle evidence.
- **Systematic TCB testing gap:** confirmed narrowly. Many negative tests already existed, but there was no field-by-field mutation campaign.
- **Automatic learning half:** confirmed. Learnings were operator-triggered only.
- **Writer confinement symmetry:** still unproven. Codex and Claude expose different sandbox mechanisms, and exact OS confinement to `owned_paths` has not been demonstrated for both.

### Correct idea, overstated wording

- Murmur did **not** lack bounded autonomy entirely: it already had bounded repairs and 1,800-second invocation/check timeouts. What it lacked was a task-wide deadline, terminal-rerun discipline, and no-progress detection.
- Murmur did **not** lack verifier adversarial tests: stale trees, forged sessions/authors, missing evidence, symlinks, hardlinks, sandbox escapes, and timeouts were already exercised. The missing piece was systematic mutation coverage.
- Cross-vendor review is useful diversity, not statistical independence. Correlated-error research shows that vendor count substantially overstates effective independent votes.
- The check Seatbelt fails closed for file/network/secret boundaries if `sandbox-exec` is absent, but its policy begins with `allow default`; it should not be described as a pure capability allowlist.

### Rejected as a blocking implementation

- **One live Codex/Claude trial in normal PR CI:** rejected. Capability evals are stochastic and initially expected to fail; only deterministic harness/grader tests or individually graduated near-100%-stable regressions should block product PRs.
- **Absolute RSS/latency gate on GitHub's hosted macOS runner:** rejected. The current M1 hosted runner has 7 GB RAM and VM noise; it is not representative of Murmur's intended local-AI profile. Exact memory/lifecycle invariants are reliable in PR CI; full physical footprint and Metal residency are not.
- **Cost/token observability:** intentionally skipped per operator decision. Subscriptions remove the immediate spend-control motivation. Wall-clock and repair/no-progress caps remain because subscriptions do not prevent runaway time.

## Implemented hardening

### 1. Real, cross-vendor production receipts only

`resolve_task_vendors` and `validate_vendor_separation` now enforce:

- public vendors are only `claude` and `codex`;
- writer and reviewer must differ;
- default is Claude writer -> Codex reviewer;
- `fake` exists only through an explicit in-process selftest interface.

The canonical verifier, finish guard, and commit path reject legacy fake receipts. The hook selftest proves both sides: a production hook blocks the fake receipt, while the private selftest path can still exercise the lifecycle.

### 2. Canonical, runner-owned risk evidence

`config.json` now maps risk evidence IDs to exact commands in `canonical_checks`. `cmd_init` injects missing profiles and rejects a caller-supplied canonical ID with a different command. `run_task` and `verify_attestation` revalidate the binding.

The previous semantic hole (`lock-negative::true`) is therefore closed. Lock/egress tasks require the exact Rust library gate; protocol changes also require the sibling protocol gate; runtime/UI paths retain their runtime/Playwright evidence.

### 3. Bounded autonomy and clean landing

The loop now adds:

- a 7,200-second task-wide deadline over writers, checks, and reviews;
- per-process timeouts capped by the remaining task deadline;
- `BLOCKED/no progress` when two consecutive repair rounds have the same staged diff and failing-check/review signature;
- rejection of rerunning any terminal task and fail-closed `BLOCKED/interrupted` landing for an abandoned nonterminal run, preventing a fresh repair budget from repeated `run` calls or process restarts;
- a content-free `learning-candidate.json` for failed/blocked tasks, requiring explicit curation rather than auto-editing binding instructions.

### 4. Verifier TCB mutation campaign

The deterministic selftest now mutates, one at a time, contract binding, tree/diff hashes, writer round/artifact/invocation/log hashes, check command/stdout/sandbox hash, review diff/artifact/log hash, and receipt timestamp. Every mutation must convert an accepted receipt into denial.

This supplements, rather than replaces, the existing adversarial cases for scope, stale state, filesystem aliases, identity, sandbox, and missing evidence.

### 5. Performance-sensitive path contract

A new `performance` risk is automatically inferred for audio, transcribe, embed, sidecar, pipeline, thermal, brain-sidecar, and measurement surfaces. It injects the canonical `perf-contracts` check.

`.agents/harness/checks/perf-contracts.sh` gates deterministic invariants in:

- `audio::recorder::tests` (resident ceiling, duration cap, muted/unmuted bounds);
- `audio::spill::tests` (durable-prefix ownership, bounded ring, stalled writer, Stop completeness/salvage);
- `perf::tests` (one-heavy-inference lane and ownership);
- `thermal::tests` (degrade/recovery policy);
- shell syntax of the physical-footprint tracer.

It deliberately does not claim to prove real signed-app RSS, physical footprint, Metal residency, ScreenCaptureKit, or thermals.

### 6. Remote-policy evaluator hardened, remote state unchanged

The desired policy now requires:

- the full CI context bound to the GitHub Actions app ID `15368`;
- strict status checks;
- at least one approving review;
- admin enforcement and conversation resolution;
- no force push or branch deletion;
- secret scanning and push protection.

The evaluator has deterministic PASS and RED fixtures and runs near the start of `scripts/ci.sh`. The **live** network audit remains a separate operator/scheduled command because harness checks are network-denied and GitHub administration reads require an explicit credential.

The 2026-07-22 live audit is still `FAIL`: only force-push and deletion protection currently pass. No remote setting was changed.

### 7. Documentation made narrower and honest

The README now distinguishes:

- file/network containment from a pure capability allowlist;
- parent-writable evidence from unreadable evidence;
- cross-vendor diversity from independent ground truth;
- deterministic PR gates from manual/scheduled stochastic capability evals;
- deterministic hot-path contracts from signed-Mac empirical measurement.

## Verification performed

- `scripts/agent-config-audit --ci` — PASS (120 checks; four documented adapter-drift warnings).
- `scripts/agent-harness selftest --ci` — PASS.
- `bash .codex/hooks/selftest.sh` — PASS (136 assertions).
- `scripts/agent-harness eval selftest` — PASS.
- `scripts/agent-remote-audit --selftest` — PASS.
- public `init --agent fake --reviewer fake` — rejected with exit 2.
- public Claude/Claude task — rejected with exit 2.
- `.agents/harness/checks/perf-contracts.sh` — PASS: 11 recorder, 24 spill, 8 perf, and 6 thermal tests.
- live `scripts/agent-remote-audit --json` — expected `FAIL`, accurately reporting the unresolved server-side controls.

An independent adversarial-verifier returned **PASS** for the focused harness claims. Its RED-before-GREEN probe first reproduced a deleted-`state.json` budget-reset bypass in a temporary copy; the final implementation blocks that task as `interrupted`, emits a learning candidate, and the regression selftest passes. The verifier found no remaining functional counterexample in the focused local harness.

The full application CI was not represented as green: this dirty checkout contains extensive concurrent product work, and earlier full-gate execution reached unrelated application warnings. Harness-focused evidence is not application certification.

## Remaining gaps and next decisions

### Operator authorization required

Configure GitHub rules/branch protection and security settings to match `remote-policy.json`. This is the highest remaining leverage because local hooks are bypassable by design. Prefer an app-bound ruleset/branch rule with no force push/deletion and no administrative bypass.

### Next medium-size harness slice

1. Add 10-15 sanitized incident-derived regression tasks with reviewed reference and deliberately bad solutions.
2. Add 2-3 pinned real-repo eval tasks and run writer/reviewer pairs in both directions for at least three trials outside blocking CI.
3. Measure marginal reviewer recall, false positives, and error overlap before adding more reviewers.
4. Add real adapter confinement canaries for both vendors: owned-path scope, credential denial, no egress, reviewer immutability, and process cleanup.
5. Promote sanitized real failures into the offline regression bank; never ingest vault/audio/transcript/provider content.

### Signed-Mac performance lane

Establish a controlled baseline matrix (Mac/RAM/macOS/Whisper/brain/Brain Live state), then run three cold signed-app cycles: idle -> 30-minute Record -> Stop -> five-minute tail. Until repeated controls define a noise envelope, judge plateau/no hidden brain/no duration-proportional post-ring growth qualitatively and preserve the raw numeric trace.

## Sources

- Anthropic, [Demystifying evals for AI agents](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents).
- Anthropic, [Effective harnesses for long-running agents](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents).
- OpenAI Agents SDK, [Running agents and max turns](https://openai.github.io/openai-agents-python/running_agents/).
- OpenAI, [Harness engineering](https://openai.com/index/harness-engineering/).
- OpenAI, [SWE-Lancer](https://openai.com/index/swe-lancer/).
- OpenAI, [Separating signal from noise in coding evaluations](https://openai.com/index/separating-signal-from-noise-coding-evaluations/).
- Apple ML Research, [Correlated errors in LLM evaluation panels](https://machinelearning.apple.com/research/correlated-llm-evaluation-panels).
- Kim et al., ICML 2025, [Correlated errors across language models](https://proceedings.mlr.press/v267/kim25e.html).
- SLSA v1.2, [Build track basics](https://slsa.dev/spec/v1.2/build-track-basics) and [provenance trust model](https://slsa.dev/spec/v1.2-rc2/build-provenance).
- Anthropic Alignment, [Automated auditing games](https://alignment.anthropic.com/2025/automated-auditing/).
- Criterion.rs, [FAQ on virtualized CI benchmarks](https://bheisler.github.io/criterion.rs/book/faq.html) and [analysis process](https://bheisler.github.io/criterion.rs/book/analysis.html).
- GitHub, [hosted-runner specifications](https://docs.github.com/en/actions/reference/runners/github-hosted-runners), [ruleset rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets), and [self-hosted-runner security](https://docs.github.com/en/organizations/managing-organization-settings/disabling-or-limiting-github-actions-for-your-organization).
- Recent preprints used only as supporting, not sole, evidence: [False Success](https://arxiv.org/abs/2606.09863) and [Infinite Agentic Loops](https://arxiv.org/abs/2607.01641).

## Open questions

- Is a second human reviewer reliably available for every PR, or should approval initially be CODEOWNERS-only on governance surfaces?
- Which historical Murmur incidents can be sanitized into public fixtures without leaking user content or implementation answers?
- What fixed Mac/configuration should own the signed performance baseline?
- Should same-vendor review ever receive a break-glass override? Current implementation intentionally says no.
