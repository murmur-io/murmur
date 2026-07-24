# Configurable harness writer/reviewer pairing — design

- **Date:** 2026-07-24
- **Status:** Approved (design); implementation plan pending
- **Author:** QueaT
- **Builds on:** the opt-in harness (PR #441, merged to `murmur` as `d6ed69e`).
- **Topic:** Let the operator choose the writer/reviewer vendor pair for a harness task — any of `claude→codex`, `codex→claude`, `claude→claude`, `codex→codex` — and make same-vendor a configurable default. Preserve the reviewer's fresh-session independence regardless of pair.

---

## 1. Problem

The harness already lets you pick the writer (`agent-harness init --agent <codex|claude>`) and reviewer (`--reviewer <codex|claude>`) per invocation, so `claude→codex` (default) and `codex→claude` already work. The ONE thing blocked is **same-vendor** (`claude→claude`, `codex→codex`), hard-rejected in three places, and the config default is not fully expressible (only `default_writer` exists; the reviewer is derived as "the other vendor"). The operator wants all four pairs available and wants same-vendor as a configurable default — the harness should be able to run entirely on Claude (no Codex dependency), which was a source of release friction.

Current rejections of same-vendor:
- `task_runner.py:254-255` `resolve_task_vendors` — `if reviewer == writer: raise "writer and reviewer must use different vendors"`.
- `task_runner.py:268-269` `validate_vendor_separation` — same raise (used during attestation validation).
- `hook_guard.py` `_validate_provenance` — writer/reviewer must differ.
- `config_audit.py:130` invariant — `default_writer == "claude"` (hard-pins the default writer; no `default_reviewer` concept).

## 2. Decision

Allow **any vendor pair**, including same-vendor. Ship a **`claude→claude` default** (two separate Claude sessions). All four pairs remain selectable per-invocation via `--agent`/`--reviewer`. The reviewer is **always a fresh, independent session with no writer context** — this is the non-negotiable property that keeps same-vendor review a real adversarial check, and it is already enforced independently of vendor.

Accepted trade-off: a same-vendor default loses cross-model-family diversity (Codex caught failure modes Claude misses and vice-versa). The operator accepts this for Claude self-sufficiency; session independence is retained.

## 3. Design

### 3.1 Config (`.agents/harness/config.json` + `schemas/`)
- Add an explicit **`default_reviewer`** field alongside `default_writer`. New shipped values: `default_writer: "claude"`, `default_reviewer: "claude"`.
- Update the config JSON schema if one constrains these fields.
- `resolve_task_vendors` (`task_runner.py:231`): 
  - `writer = requested_writer or config["default_writer"]`
  - `reviewer = requested_reviewer or config.get("default_reviewer") or {"codex":"claude","claude":"codex"}[writer]` (the final clause keeps backward compatibility if `default_reviewer` is ever absent).
  - Keep: both must be real vendors (`codex|claude`); the `fake→fake` selftest-only path is unchanged.
  - Remove: the `reviewer == writer` rejection.

### 3.2 Relax the separation checks
- `resolve_task_vendors` (`task_runner.py:254-255`): delete the `if reviewer == writer: raise`.
- `validate_vendor_separation` (`task_runner.py:259-269`): delete the `if writer == reviewer: raise`; keep the "production tasks may use only codex and claude" real-vendor guard. Rename to `validate_model_vendors` (the function no longer validates separation) — update all call sites.
- `hook_guard.py` `_validate_provenance`: delete the writer≠reviewer rejection; keep "both writer and reviewer vendors ∈ {codex, claude}" and the writer/reviewer-vendor-match-the-contract checks.

### 3.3 `config_audit`
- Invariant at `config_audit.py:130` (`default_writer == "claude"`) → replace with: `default_writer ∈ {codex, claude}` **and** `default_reviewer ∈ {codex, claude}` (require `default_reviewer` present). The `harness-config` fingerprint changes (expected). No parity impact (`config.json` is not cross-vendor-mirrored).

### 3.4 Preserved invariants (unchanged — verify, don't weaken)
- **Fresh independent reviewer session:** every review's `session_id` must exist, must differ from the writer session, and must be unique across reviews (`hook_guard.py` `_validate_attestation` review loop + `task_runner.py` `verify_attestation`). This is vendor-agnostic and is what makes `claude→claude` a genuine adversarial review rather than self-grading. It stays exactly as-is.
- **Reviewer runs read-only:** enforced in the run loop and by the per-role sandbox/eval config. A Claude reviewer must be read-only just like a Codex reviewer — verify during implementation (the run loop enforces read-only by role, not vendor).
- **Rest of the attestation contract** (hash-binding, checks, risk-reviews, required reviews, rounds, timestamps) is untouched.

### 3.5 Docs
- `.claude/skills/harness/SKILL.md` + `.agents/skills/harness/SKILL.md` (keep byte-identical): document `--agent`/`--reviewer`, the four selectable pairs, the new `claude→claude` default, and the trade-off note (same-vendor = less family diversity, still fresh-session independence).
- `AGENTS.md`: update the Codex-facing note to reflect selectable pairs incl. same-vendor.
- `.claude/rules/agentic-workflow.md` **and** `.codex/rules/agentic-workflow.md` (byte-identical; `config_audit` enforces rule parity): update the binding text that currently says "the default vendor pair is Claude writer → Codex reviewer; the only supported reversal is Codex writer → Claude reviewer; same-vendor production review … forbidden." New text: any of the four pairs is allowed; default is `claude→claude`; the reviewer is always a fresh independent session; cross-vendor is recommended when model-family diversity matters.

## 4. Testing
- `bash .claude/hooks/selftest.sh` must stay green. Update/extend the vendor cases:
  - Flip any assertion that same-vendor (`claude→claude`, `codex→codex`) is rejected → now ACCEPTED.
  - Keep (must still BLOCK): a review whose `session_id` equals the writer session (session reuse), and any review reusing another review's session.
  - Add ALLOW coverage for `claude→claude` and `codex→codex` receipts (valid when sessions are distinct).
  - Keep the `fake→fake` selftest-only path working.
- `resolve_task_vendors` / `validate_model_vendors` unit assertions: same-vendor resolves and validates; a non-vendor (e.g. `fake` in production) still rejects.
- `scripts/agent-config-audit --ci` green with the new `default_reviewer` field.
- `scripts/agent-harness selftest --ci` green.

## 5. Rollout
- Touches `protected_paths` files (`.agents/harness/**`, `.claude/**`, `.codex/**`, `AGENTS.md`) and changes `instructions_sha256`. Land via a feature branch + PR to `murmur` (never a direct push). Commit from a normal terminal or via `--kind harness`.
- After merge, the default harness run is `claude→claude` — verify once in a real `meetnotes`-rooted session that a harness task with a claude reviewer completes (fresh reviewer session, read-only, PASS attestation).

## 6. Out of scope / follow-ups
- **Per-role Claude model tiering** (e.g. writer=sonnet, reviewer=opus) to add intra-vendor diversity to `claude→claude`. Deliberately not built now (YAGNI); recorded as a possible future.
- No change to the opt-in switch, secret-scan, trunk protection, resource-lane, or the block-bash fixes from PR #441.
