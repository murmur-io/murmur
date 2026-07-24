<!-- Generated 2026-07-24 via a 14-agent Workflow (5 grounding readers over the real files + 6 web researchers on Anthropic/OpenAI/industry canon -> structured synthesis -> adversarial critique -> reconciled brief). External best-practice claims are point-in-time (2026-07). -->
# Assessment: Murmur's agent-work control-plane vs. best practices

## Method
5 grounding subagents read the actual setup (`task_runner.py`, `config.json`, hooks, prompts, evals, agents, skills, rules, resource scripts, memory/learnings); 6 web researchers pulled the Anthropic / OpenAI-Codex / industry canon; a synthesizer scored 12 dimensions; an adversarial critic stress-tested the synthesis; a final pass reconciled them. The reconciled brief follows.

## Independently re-verified before publishing (trust code, not prose)
- **CLAUDE.md agent roster lists 7 of 11** agents (missing: `ai-systems-architect`, `app-perf-engineer`, `memory-retrieval-architect`, `model-perf-engineer`). -- `CLAUDE.md:117` vs `ls .claude/agents/`.
- **Learnings trees have diverged:** `.claude/learnings` = 92K vs `.codex/learnings` = 68K (same filenames, different bytes); `/learn` writes the non-injected `.claude` mirror while `learning_prompt()` injects only the canonical `.codex` tree.
- **The 7-bug catalogue has different membership** in `.claude/rules/agentic-workflow.md` (...Opacity bleed / CSP-T4) vs the harness `adversarial-reviewer.md` (...EGRESS_WITHOUT_CONSENT / PROCESS_OWNERSHIP_KILL).
- **Survivor-guardian** appears **6x** in `scripts/agent-resource-run`, **0x** in `scripts/agent-dev-run`.
- **The telemetry gap is a missing-analytics-layer, not "total absence"**: per-check `duration_ms` + `events.jsonl` already exist; the rollup (cost/queue-wait/block-reason) does not.

---

# Murmur Agent Harness — Final Decision-Ready Assessment

*Reconciles the multi-dimensional synthesis with the adversarial critique. Where the critic is right, the synthesis is corrected below; where the critic overreaches, that is flagged explicitly.*

---

## 1. Verdict

This is an exceptionally rigorous, honestly-documented agent harness that meets or beats the published 2026 frontier on the axes that actually catch bugs: a deterministic non-LLM runner owns "done" (the model has no code path to write `state.json`), independent **cross-vendor** review de-correlates the self-preference channel the literature documents, deterministic checks run fail-closed under per-check Seatbelt with no secret reads, byte-exact runner-owned risk evidence can't be satisfied with a label, and a recompute-don't-trust hash-bound attestation ports in-toto/SLSA provenance to the *development* step — a combination almost no public harness has. The critique is right on two systematic points, and I have adjusted for both: (a) the synthesis scored the **ceiling** (what a green attestation proves *when invoked*) rather than the realized **floor** — the whole apparatus is opt-in, remotely unenforced, and the operator's own blessed release recipe documents standing bypass; and (b) most items labeled "ahead-of-frontier" are disciplined *adoption* of already-published best practice (cross-vendor judging, Seatbelt, runner-owns-done, in-toto shape), which is ahead of what almost anyone *ships* but at-par with the frontier of *knowledge* — only two items are genuinely novel. The complexity is largely *earned* (unrecoverable failure modes: data loss, privacy breach, launch abort) but its ROI rests on an uncontrolled catch-count with no false-negative denominator, so it is **plausibly**, not demonstrably, worth its cost.

**Calibrated score: 7.5 / 10** (synthesis said 8.3). Split honestly: **~8.5 as invocation-time capability**, **~6.5 as realized average per-commit rigor** given the opt-in/bypassable design, blended to 7.5. This is a high score. The correct conclusion is *mostly leave the trust-plane alone* — the remaining work is measuring and maintaining what exists, not adding more rigor.

---

## 2. Where it exceeds the frontier

### Genuinely novel (beyond the published state of the art)

- **Recompute-don't-trust attestation ported to the development step.** `verify_attestation` (`.agents/harness/task_runner.py`, ~L3171) re-derives `tree_sha`, the staged diff, the Seatbelt **profile text**, and the env allowlist+values from Git and the filesystem and requires them to still match — ~60 sequential `require()`s. in-toto/SLSA statements are normally *build* provenance; applying the tamper-evident "recompute, never trust" shape to the act of an LLM editing code is uncommon in any public harness. *Beats:* SLSA build-provenance convention, which almost nobody extends to the dev step.
- **Byte-exact runner-owned canonical risk evidence, re-derived from staged paths at commit.** `config.json` risk tables + `hook_guard.py::_classify_actual_risks`/`_validate_attestation` recompute `actual_risks` from the **actual staged paths** (not the declared `risk_flags`), require the declared set to be a superset, and pin each evidence command byte-for-byte (`--check 'rust-lib::true'` is rejected). *Beats:* OpenAI-style per-tool low/med/high risk labels — here the label is un-fakeable because the evidence is a real, sandboxed, hash-bound run of the exact canonical command derived from the diff itself.

### At the frontier, ahead of what almost anyone ships (disciplined adoption)

- **Runner-owns-"done" state machine** (`task_runner.py::run_task`): the model appears only as writer/reviewer inside the loop and cannot declare completion. This is precisely the Nov-2025 *effective-harnesses* recommendation, cleanly implemented — structurally beating Anthropic's prompt-level "self-verify before marking done."
- **Cross-vendor adversarial review, enforced at three layers** (`resolve_task_vendors` → `validate_vendor_separation` → `hook_guard._validate_provenance`), fake stub forbidden in production, every review `session_id` distinct. **Corrected per critique:** this *reduces* the self-recognition channel (Panickssery et al., NeurIPS 2024) — it does not "structurally sever" it. Two LLMs share training-corpus/RLHF blind spots, and each review is a single stochastic trial with no k-of-n; "adversarial PASS" is a de-correlated but still-fallible model opinion.
- **Fail-closed Seatbelt per deterministic check** with mach-lookup denied to `securityd`/keychain and profile+env SHA-bound (`build_check_seatbelt_profile`). **Corrected per critique:** this is ahead of typical agent sandboxes on the *no-ambient-secrets* axis, but the filesystem layer is a **denylist** `(allow default)+denies` on the **deprecated** `sandbox-exec` primitive, and it wraps the *checks*, not the editing *agent* (only network is true default-deny). Not "ahead-of-frontier" — at-par with a real caveat.
- **Self-validating eval oracle** (`eval_runner.py` + `evals/fixtures/*/{initial,good,bad}`): the fake-adapter selftest proves each grader discriminates the exact production-bug mechanism *before* any live number is trusted, with `false_green` first-class — more discipline than most eval harnesses.
- **flock-on-git-common-dir resource lane** (`scripts/agent-resource-run`) with a fail-closed survivor guardian, plus the dev-server per-rustc PATH proxy (`scripts/agent-dev-run`). **Corrected per critique (verified):** the survivor guardian exists **only** in `agent-resource-run`; `agent-dev-run` duplicates ~90 supervisor lines *without* the fail-closed survivor path. Careful bespoke single-Mac plumbing, not a frontier axis.
- **Remote-audit least-privilege token hygiene** (`scripts/agent-remote-audit.py`, `scripts/ci.sh`): two distinct tokens, secrets `unset` before any repo code runs, fail-closed no-fallback (itself CI-enforced), `MONITOR_ONLY` honesty, an 11-mutation non-vacuous selftest. Exemplary.
- **`config_audit.py` meta-enforcement** of hook-wrapper byte-parity, the no-fallback token flow, and that the adversarial prompt still names the shipped bug classes. **Corrected per critique:** strike "policy can't quietly rot." It meta-enforces the *mechanical* seams; the *prose* control-plane (bug catalogue, agent roster, learnings entrypoints, duplicate skill trees) is **un-audited and has already drifted** (see gap P1-c).

---

## 3. Scorecard

| Dimension | Verdict | One-line note |
|---|---|---|
| Independent review / verification | **At frontier** | Cross-vendor de-correlates self-recognition; single-trial, no k-of-n — a PASS is a model opinion, not a proof. |
| Isolation / hermeticity / reproducibility | **Ahead of typical** | Fail-closed, hash-bound, no-secret-reads — but a denylist on deprecated `sandbox-exec`, wrapping checks not the agent. |
| Provenance / attestation (non-falsifiability) | **Novel** | Recompute-not-trust ported to the dev step; only deficit is self-generated/unsigned = SLSA L1, not L2/L3. |
| Risk routing / security-gating | **Novel-ish** | Un-fakeable byte-exact evidence re-derived from the diff; blind to sensitive surface in *unlisted* new paths. |
| Eval harness coverage | **Partial gap** | Excellent as a *harness-correctness* gate; thin as a *capability* benchmark; pass@1 default; 4 top bug-classes uncovered. |
| Context engineering (rules/skills/memory) | **At par** | Expert JIT retrieval + subagent isolation; only exposure is scoping (rules always-resident), not content. |
| Orchestration control-flow | **At frontier** | Deterministic manager owns lifecycle; the one clean gap vs LangGraph is no crash-resume. |
| Resource / concurrency | **Ahead of typical** | flock lane + guardian + dev-proxy are careful; caps overlap not a single build's RAM peak; `agent-dev-run` lacks the guardian. |
| Observability / telemetry | **Partial gap (clearest)** | Per-check `duration_ms` + `events.jsonl` exist; no rollup of cost/queue-wait/block-reason. The repo's own #1. |
| Human-in-the-loop / governance | **At par** | Humans hold every irreversible step; approvals=0 = zero human second-eye; discipline remotely unenforced. |
| Portability / maintenance cost | **Partial gap** | 6k-line single-file engine + macOS-only primitive + a second prose-codebase already drifting. |
| Memory / learning loop | **At par** | Injection half is closed and hash-bound; write half is human-dependent + a live wrong-entrypoint drift. |

---

## 4. Real gaps, prioritized

**There are no P0s.** The system is *correct*; every gap below is about efficiency, coverage, or maintenance — not a bug that produces a wrong verdict. Stating that plainly is itself a finding: the trust plane is done.

### Genuinely worth doing

**P1-a — No per-stage cost/timing telemetry (aggregation layer).** *Effort M.*
Evidence: **corrected per critique (verified)** — per-check `duration_ms` + `started_at`/`finished_at` and a state-transition `events.jsonl` *do* exist (`task_runner.py`); the resource lane's lock file holds `{pid, cwd, acquired_epoch}`. What is missing is the **rollup**: queue-wait, writer/reviewer wall-clock, token/cost, profile/risk selection, terminal block-reason. It is a *missing-analytics-layer* gap, not the "total absence" the synthesis claimed — still the correct #1, and the repo's own `docs/research/2026-07-24-harness-fast-lanes.md` ranks it first. This is the prerequisite for answering the economics question (§5) and for detecting contention false-blocks.
Recommendation: emit an OTel-GenAI-shaped per-task telemetry rollup.

**P1-b — Eval is a harness-correctness gate, not a capability benchmark.** *Effort L.*
Evidence: 11 single-file toy fixtures; `--trials` defaults to 1 (so pass@k = pass@1, `FLAKE` never fires); the two strongest graders degrade to gameable string-counting when `rustc` is absent (`smoke.py`); and four highest-severity shipped bug-classes — `FFI_LAUNCH_ABORT`, `ANGULAR_IMPORT_CYCLE_ɵcmp`, `EGRESS_WITHOUT_CONSENT`, CSP-T4 style-break — have **no fixture**, guarded only by words in the reviewer prompt. No threshold/trend wiring, so a model/CLI capability regression passes silently (the *under*-alerting mirror of the SWE-bench collapse).
Recommendation: add the 4 fixtures; make rustc-behavioral graders mandatory (HARNESS_FAIL if `rustc` absent, never string-count); default live trials ≥3 reporting **pass^k** (reliability), not pass@1; add a scheduled trend gate. *Only worth it if you want the eval to be a regression detector — as a self-check that the instrument works, it is already well-built and correctly CI-gated.*

**P1-c — Control-plane prose is multi-sourced and already drifting.** *Effort M.*
Evidence (all verified in-context): the 7-bug catalogue **disagrees** between `agentic-workflow.md` and `adversarial-verifier.md` (the latter splits the leak into two and drops CSP-T4 entirely); `CLAUDE.md`'s roster lists **7 of 11** agents (`ai-systems-architect`, `app-perf-engineer`, `memory-retrieval-architect`, `model-perf-engineer` absent); `.agents/skills` duplicates `.claude/skills` and drifts by bytes; `/learn` writes the **non-injected** `.claude/learnings` mirror while the canonical injected tree is `.codex/learnings` (~24KB vs ~11KB divergence — a lesson via the wrong entrypoint is **never injected**). This is exactly the drift the repo's own "trust code, not docs" motto warns about, occurring *inside the control plane*.
Recommendation: single-source the bug catalogue and roster (one referenced file each); converge `/learn` onto `.codex/learnings` (or make `config_audit` FAIL on the divergence); add `config_audit` checks for roster completeness + skill-tree parity so drift becomes a CI failure.

### Nice-to-have

**P2-a — Risk classification is a denylist blind to genuinely new surface.** *Effort M.*
Evidence: `risk_classification` is hand-maintained path-globs; a new `src-tauri/src/telemetry.rs` doing network egress matches no `egress` glob and auto-injects no specialist; `protocol` under-reaches the real sibling-repo `murmur-protocol` crate. Fails closed on *relocation*, not on new surface.
Recommendation: fail-closed catch that a new top-level module (or a new network/keychain syscall) forces manual risk triage; extend the `protocol` glob + a cross-repo hook.

**P2-b — Fake-adapter production safety rests on a single boolean (`allow_test_adapter`).** *Effort S.* (Critique-surfaced miss.)
Evidence: the entire barrier between a canned-PASS stub reviewer and production self-certification is that flag being set only by the in-process selftest path. Currently gated correctly, but one refactor or leaked flag collapses the "implementer never owns the verdict" guarantee.
Recommendation: add a second independent assertion (e.g. the commit-time guard already re-checks; make the fake-vendor rejection defense-in-depth explicit and unit-tested).

**P2-c — Rigor is inversely correlated with stakes at release.** *Effort S.* (Critique-surfaced miss — the sharpest one.)
Evidence: the blessed release recipe (`release-under-harness-sandbox` memory) runs sign/notarize/publish — the highest-stakes *irreversible* steps — with `dangerouslyDisableSandbox` + `allowUnsandboxedCommands` + `finish-guard=advisory`. The governance layer credits "humans hold every irreversible step," but those exact steps run with the *least* harness protection.
Recommendation: make the release-time posture a conscious, logged choice; consider keeping `finish-guard` non-advisory for the release-chore commit itself even when the heavy commands are unsandboxed.

### Lower urgency

**P3-a — 6,274-line single-file engine, unverified verifier-of-verifiers, no crash-resume.** *Effort L.* `verify_attestation` is a 340-line straight-line `require()` list where one omitted check is a silent hole; the in-file selftest is written by the same author; a mid-run crash burns the whole 2h budget. Recommendation: extract + independently unit-test `verify_attestation` with a coverage check that every attestation field has a matching `require()`; add checkpoint/resume.

**P3-b — Attestation is self-generated and unsigned (SLSA L1).** *Effort M.* Whoever controls the runner could in principle forge it. **Critic note (partly agreed):** for a local, single-operator, no-CI-builder app whose trust root is already "the Python runs faithfully + Git is honest" and whose authoritative boundary is the remote required CI check, a DSSE signature adds little practical assurance. Correctly P3; **do not prioritize** unless you add a CI builder.

**P3-c — `sandbox-exec` deprecation is a fail-closed-on-removal cliff, not just portability.** *Effort S to monitor.* (Critique-surfaced.) Because absent-sandbox fails **closed** (BLOCKED, no unsandboxed fallback), a future macOS release removing the deprecated primitive would **brick** the entire deterministic-check guarantee overnight. Recommendation: keep a documented fallback plan (Endpoint Security / container tier); low urgency, but know it's a single point of failure.

### Critic-downgraded (flagged as overstated in the synthesis)

- **"~13-17k tokens resident on turn zero → the #1 anti-pattern; move rulesets to child-directory `CLAUDE.md`, cuts load >50% losing nothing."** **Overstated — downgrade to a minor optimization.** Two problems the critic is right about: (1) you cannot rate this layer both "expert / at-par" *and* the named #1 anti-pattern — the rules are *dense but high-signal*, not bloated; (2) child-directory `CLAUDE.md` loads JIT only when files in that dir are **read**, but a harness writer plans *before* reading and cross-cutting changes span both `src/` and `src-tauri/`, so relocation risks the rust/lock/angular rules **not loading exactly when a change touches both trees**. "Losing nothing" is unproven and plausibly wrong. Keep the always-on rules; at most trim MEMORY.md's resident footprint.
- **"Reviewers dispatch serially" as a gap.** **Not a gap** — by its own text it is a pure latency optimization that changes nothing about what is proved. Moved to Recommendations (§6).

---

## 5. The solo-operator lens

**Is it over-engineered? Mostly no — but the ROI is asserted, not measured.** The failure modes are genuinely unrecoverable (seal content-loss, sealed-content leak, FFI launch-abort), and the fleet has a recorded catch history. The critique is right, though, that "demonstrably improved outcomes" overstates it: the catch-count has **no false-negative denominator**, and the operator's own memory records **harness escapes** — the managed-block egress leak where "4 verify cycles caught what lock-sec + green-build **missed**," plus shipped regressions the harness didn't prevent (0.3.0/0.3.1 un-notarized, the 14GB recording OOM, launch-freeze). So: **plausibly** worth its cost, not proven.

**Three real drags for one person:**
1. **Cost per change is never computed** — the single most important solo question, and it's dodged. A full pass = writer + spec + adversarial + auto-injected specialists + up to 3 repairs, each a heavy always-compiled ML build serialized on one machine-wide flock. Nobody has measured what one harness-gated change costs in tokens, wall-clock, and operator friction. **This is why P1-a (telemetry) is the top move** — you cannot make the over-engineering call without it.
2. **The verifier-of-verifiers is itself unverified.** The engine enforcing "the implementer never owns the verdict" is a 6k-line single file with no separate test suite and an in-file selftest by the same author — by the harness's own doctrine it warrants independent cross-vendor review and receives none. That is the recursive bus-factor, not merely "file-size fragility."
3. **A second prose-codebase that must track a fast-moving `src-tauri/`** — and it is *already* drifting (P1-c). This is the maintenance liability that compounds silently.

**Does it get used? Yes for features, no floor for the rest.** The memory is full of real "adversarial + lock-sec PASS" on feature/fix PRs, so the machinery is genuinely exercised where it matters most. But it is **opt-in, remotely unenforced, and the release path specifically opts out** — the enforceable floor is only "green `ci.sh` + PR + no force-push/deletion + resolved threads," approvals=0 means zero human second-eye, and the coarse guards generate false positives that create standing pressure to disable them. The realized *average* per-commit rigor is well below the ceiling the machinery can prove.

**When to STOP:** now, for the trust plane. The provenance/isolation/gating/cross-vendor machinery is complete and among the best anywhere — **do not add DSSE, more reviewers, more gates, or a bigger eval.** "It's already excellent, don't add more" is the correct answer for that half. The *only* additions worth making are the ones that make what exists **cheaper to run (telemetry) and cheaper to maintain (single-sourcing)** — i.e. reduce drag, don't add rigor.

---

## 6. Recommended next moves (small-first, mirroring the repo's own fast-lanes doc)

1. **Telemetry first (S→M).** Per `docs/research/2026-07-24-harness-fast-lanes.md` #1: emit a per-task rollup — queue-wait, writer/check/reviewer wall-clock, tokens/cost, profile/risk selection, terminal block-reason — OTel-GenAI-shaped. Unlocks the economics answer and surfaces contention false-blocks (e.g. the version-bump BLOCKED on smoke-port contention). *Highest leverage; everything evidence-driven depends on it.*
2. **Parallel reviewer fan-out (S).** spec/adversarial/specialist are read-only observers of one immutable `staged_diff_sha256` — dispatch them concurrently. Parallelize *reviewers*, never the Rust lane. Pure latency win; nothing proved changes.
3. **Release-version profile / release-time posture (S).** Adopt the fast-lanes release-parity idea, and make the release-time bypass (P2-c) a conscious, logged decision rather than an implicit one.
4. **Single-source the drift surfaces (M).** One bug-catalogue file, one roster file; converge `/learn` onto canonical `.codex/learnings` (or make `config_audit` FAIL on the split); add roster-completeness + skill-tree-parity audits. Cheap insurance against the second-codebase rot that has already begun.
5. **Eval hardening (L) — only if you want a regression detector.** Add the 4 uncovered high-severity fixtures, make rustc-graders mandatory (no string-count fallback), default trials ≥3 reporting pass^k, wire a scheduled trend gate.
6. **Optional, low-urgency.** Extract + unit-test `verify_attestation` out of the 6k module; add checkpoint/resume; DSSE-sign the attestation *only if* you ever add a CI builder. Skip otherwise — the practical assurance gain for a local solo app is small.

Do items 1–4 (all S/M, days not weeks). Treat 5–6 as opt-in. Leave the trust plane alone.
