# Review-mode eval — measure same-vendor vs cross-vendor reviewer catch-rate — design

- **Date:** 2026-07-25
- **Status:** Approved (design); implementation pending
- **Author:** QueaT
- **Builds on:** the configurable vendor-pairing change (PR #442, `30f05eb`) and the risk-based
  reviewer auto-escalation (`escalate_reviewer_for_risk`, `feat/harness-risk-escalation`).
- **Topic:** Add a `review` eval mode that presents a planted-defect diff to a *reviewer* agent and
  measures whether it returns FAIL and names the defect — run as a vendor matrix (`claude` vs
  `codex`) so the same-vendor-vs-cross-vendor catch-rate delta becomes a **measured number**, not an
  article of faith.

---

## 1. Problem

The shipped harness default is now same-vendor review (`claude→claude`); high-risk paths
auto-escalate to cross-vendor. Both the diversity *cost* of same-vendor and the *benefit* of the
escalation are currently **unmeasured**: `grep -c 'reviewer\|writer\|vendor' .agents/harness/eval_runner.py`
returns 0 — the eval harness invokes exactly one agent per trial (`run_trial → invoke_agent`) and
grades deterministically. There is **no reviewer in the loop**, so:

1. No number bounds the false-negative cost of same-vendor review vs cross-vendor.
2. The change's selftests only prove vendor *resolution/provenance* (that same-vendor is *accepted*
   and escalation fires), never that a Claude reviewer *catches* what a Codex reviewer would.
3. Four shipped high-severity bug classes have no writer fixture either — `FFI_LAUNCH_ABORT`,
   `ANGULAR_IMPORT_CYCLE_ɵcmp`, `EGRESS_WITHOUT_CONSENT`, and the CSP-T4 style break — so they are
   guarded only by words in `adversarial-reviewer.md`.

## 2. Decision

Add a **`review` eval mode** that reuses the existing `initial/good/bad` fixture overlays. For each
fixture:
- present the **`bad`** overlay (a planted defect) to a reviewer agent as "the writer's staged
  diff", using the neutral `adversarial-reviewer.md` prompt and the `review` JSON schema;
- **catch** = the reviewer returns `verdict: FAIL` **and** its rationale names the defect mechanism
  (grader keyphrase set per fixture);
- present the **`good`** overlay as an **over-block control**: a FAIL there is a false positive;
- run the matrix `reviewer ∈ {claude, codex}` × `trials ≥ 3` and report per-vendor catch-rate,
  false-positive-rate, and the **cross-minus-same delta**.

This is a **manual/scheduled live trial**, never a blocking PR gate (same policy as the existing
capability trials — one stochastic trial must never red-bar a product PR).

## 3. Design

### 3.1 Task metadata (`.agents/harness/evals/tasks/*.json`)
Add an optional `"mode": "review"` (default `"write"` preserves today's behavior) and, for review
tasks, a `"catch_keyphrases": [...]` list (defect-naming terms) and a `"defect_class"` tag (one of
the 7 shipped bug classes). Reuse the existing `fixtures/<name>/{good,bad}` overlays already present
for `lock-masked-dto`, `seal-verify-before-destroy`, etc. Extend `validate_task` to accept the new
fields and require `catch_keyphrases` when `mode == review`.

### 3.2 Runner (`eval_runner.py`)
- `run_trial` branches on `task["mode"]`. For `review`:
  - build the diff-under-review by diffing `initial → bad` (catch case) and `initial → good`
    (control case);
  - dispatch a reviewer via the existing `invoke_agent` path but with the reviewer role prompt
    (`read_prompt("adversarial-reviewer")`) + the `review` schema, and the diff embedded as
    untrusted evidence (the prompt already treats task/diff/logs as untrusted);
  - the reviewer runs **read-only** (assert no workspace mutation, reusing the existing
    read-only-review assertion pattern in `task_runner.py`).
- New grader `grade_review(response, task, case)`:
  - `case == "bad"`: catch iff `verdict == FAIL` and any `catch_keyphrases` term appears in the
    rationale (case-insensitive, whole-word); a PASS verdict = miss (false negative).
  - `case == "good"`: over-block iff `verdict == FAIL` (false positive).
  - `HARNESS_FAIL` (not a silent string-count) if the reviewer CLI/model is unavailable — never
    fabricate a catch/miss.
- `compute_metrics` gains: per-vendor `catch_rate`, `false_positive_rate`, `n`, and a top-level
  `cross_minus_same_delta` per fixture and aggregate.

### 3.3 CLI
`scripts/agent-harness eval run --mode review --reviewer <codex|claude> --trials 3 [--suite review]`.
A `review` suite lists the review-mode tasks. `eval report <run-id>` prints the catch-rate matrix.

### 3.4 New fixtures (covers the 4 unrepresented classes)
Add `bad/good` overlays for: `ffi-launch-abort` (an `msg_send!` of an unproven selector),
`angular-import-cycle` (mutually-recursive standalone components missing `forwardRef`),
`egress-without-consent` (a provider/tool egress bypassing the consent/redaction/ledger seam), and
`csp-style-nonce` (a `style-src` nonce re-introduced into `tauri.conf.json`). Each `bad` is a
minimal single-file plant; `good` is the corrected form; `catch_keyphrases` name the mechanism.

## 4. Testing
- Deterministic selftest: with the **fake** reviewer adapter scripted to (a) return
  `FAIL + keyphrase` and (b) return `PASS`, assert `grade_review` scores catch vs miss correctly,
  and that a missing reviewer CLI yields `HARNESS_FAIL` not a fabricated result. This is the only
  part that belongs in blocking CI.
- Live matrix (`reviewer ∈ {claude, codex}`, `trials ≥ 3`) is manual/scheduled; record catch-rate
  and the cross-minus-same delta in the run report.

## 5. Rollout / interpretation
- Touches `protected_paths` (`.agents/harness/**`); land via PR to `murmur`, never a direct push.
- **Interpretation guard:** a small measured delta does *not* prove same-vendor is safe on high-risk
  paths — the research is explicit that family-level self-preference is highest on *confidently
  wrong* outputs, which are exactly the ones a same-family reviewer is least likely to flag and a
  single eval sample under-represents. Treat the delta as a floor on the diversity benefit, and keep
  the `escalate_reviewer_for_risk` policy regardless of the measured number.

## 6. Out of scope / follow-ups
- k-of-n multi-reviewer voting (would also address the "single stochastic trial, no k-of-n" caveat).
- Per-role Claude model tiering (writer=sonnet / reviewer=opus) as a Codex-free partial-diversity
  lever — measure its delta here once tiering exists.
