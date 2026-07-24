# Configurable Harness Writer/Reviewer Vendor Pairing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow any writer/reviewer vendor pair for a harness task (`claude→codex`, `codex→claude`, `claude→claude`, `codex→codex`), ship a `claude→claude` default via a new `default_reviewer` config field, and keep the reviewer's fresh-session independence and read-only posture regardless of pair.

**Architecture:** Remove the three `writer == reviewer` rejections (`resolve_task_vendors`, `validate_vendor_separation`→renamed `validate_model_vendors`, `hook_guard._validate_provenance`), add an explicit `default_reviewer` to `config.json`, and relax the `config_audit` default-writer pin to validate both defaults are real vendors. The session-independence and read-only checks are untouched. Docs (skill, AGENTS.md, binding rule) updated in lockstep across both vendors.

**Tech Stack:** Python 3 (harness runner + guard + audit + their selftests), JSON (config), Markdown (docs). No new dependencies.

**Design spec:** `docs/superpowers/specs/2026-07-24-configurable-harness-vendor-pairing-design.md`

## Global Constraints

- **Commit identity:** author **only** `QueaT <kgm004a@gmail.com>`; **no** AI co-author trailers (`git -c user.name=QueaT -c user.email=kgm004a@gmail.com commit -m "..."`).
- **Never push directly to `murmur`** — feature branch `feat/harness-vendor-pairing` (already created) → PR → merge.
- **Reviewer precedence (locked):** `reviewer = requested_reviewer or config["default_reviewer"]`; keep a fallback to the opposite vendor ONLY if `default_reviewer` is absent (backward compat). Ship `default_reviewer: "claude"`, so the default is `claude→claude`.
- **Non-negotiable invariants (do NOT weaken):** the reviewer is always a fresh session (`session_id` present, ≠ writer session, unique across reviews) and read-only. These live in `_validate_attestation`/`verify_attestation` review loops and the run loop; this change must not touch them.
- **Gates:** `scripts/agent-harness selftest --ci` (runs `cmd_selftest`, covers vendor resolution), `bash .claude/hooks/selftest.sh` (hook guard), `scripts/agent-config-audit --ci` (config + parity). All must stay green.
- This change touches `protected_paths` and changes `instructions_sha256`; commit from a normal terminal or via `--kind harness`.
- No new npm/cargo dependencies; `com.meetnotes.app` untouched.

---

## File structure

| File | Change |
| --- | --- |
| `.agents/harness/config.json` | add `default_reviewer: "claude"` |
| `.agents/harness/task_runner.py` | `resolve_task_vendors` precedence + drop same-vendor raise; rename `validate_vendor_separation`→`validate_model_vendors` + drop same-vendor raise + update 2 call sites; update `cmd_selftest` vendor assertions |
| `.agents/harness/hook_guard.py` | drop the `writer_vendor == reviewer_vendor` raise in `_validate_provenance`; add a direct same-vendor provenance unit assertion to the selftest |
| `.agents/harness/config_audit.py` | relax the `default_writer == "claude"` invariant to validate both defaults are real vendors |
| `.claude/skills/harness/SKILL.md`, `.agents/skills/harness/SKILL.md` | document `--agent`/`--reviewer`, the four pairs, `claude→claude` default, trade-off note (keep byte-identical) |
| `AGENTS.md` | update Codex note to reflect selectable pairs incl. same-vendor |
| `.claude/rules/agentic-workflow.md`, `.codex/rules/agentic-workflow.md` | update the binding vendor-pairing text (keep byte-identical) |

---

## Task 1: Config default + vendor resolution/validation relaxation

**Files:**
- Modify: `.agents/harness/config.json`
- Modify: `.agents/harness/task_runner.py` (`resolve_task_vendors` ~231-256; `validate_vendor_separation` ~259-269; its 2 call sites at ~2746 and ~3177; `cmd_selftest` vendor block ~4602-4626)

**Interfaces:**
- Produces: `validate_model_vendors(contract, *, allow_test_adapter=False)` (renamed from `validate_vendor_separation`) — consumed at the two call sites and by any importer.
- Config gains `default_reviewer` (string, `codex|claude`).

- [ ] **Step 1: Update the failing vendor selftest assertions in `cmd_selftest`**

In `.agents/harness/task_runner.py`, replace the vendor block (currently lines ~4605-4621):

```python
    if resolve_task_vendors(default_cli_args.agent, default_cli_args.reviewer, config) != (
        "claude",
        "codex",
    ):
        failures.append("default task vendors are not Claude writer -> Codex reviewer")
    if resolve_task_vendors("codex", None, config) != ("codex", "claude"):
        failures.append("explicit writer override did not select the opposite reviewer")
    for writer, reviewer, label in (
        ("fake", "fake", "public fake adapter"),
        ("codex", "codex", "same-vendor Codex review"),
        ("claude", "claude", "same-vendor Claude review"),
    ):
        try:
            resolve_task_vendors(writer, reviewer, config)
            failures.append(f"{label} was accepted")
        except HarnessError:
            pass
```

with:

```python
    if resolve_task_vendors(default_cli_args.agent, default_cli_args.reviewer, config) != (
        "claude",
        "claude",
    ):
        failures.append("default task vendors are not Claude writer -> Claude reviewer")
    if resolve_task_vendors("codex", None, config) != ("codex", "claude"):
        failures.append("writer override did not fall back to the configured default_reviewer")
    # Same-vendor pairs are now allowed (session independence is enforced elsewhere).
    for writer, reviewer, label in (
        ("codex", "codex", "same-vendor Codex review"),
        ("claude", "claude", "same-vendor Claude review"),
    ):
        if resolve_task_vendors(writer, reviewer, config) != (writer, reviewer):
            failures.append(f"{label} was not accepted")
    # The public (non-selftest) fake adapter is still rejected.
    try:
        resolve_task_vendors("fake", "fake", config)
        failures.append("public fake adapter was accepted")
    except HarnessError:
        pass
```

- [ ] **Step 2: Add `default_reviewer` to the config selftest expectation and run the selftest to see it fail**

First add `default_reviewer` to config (needed for the assertions): in `.agents/harness/config.json`, add after `"default_writer": "claude",`:

```json
  "default_reviewer": "claude",
```

Run: `scripts/agent-harness selftest --ci`
Expected: FAIL — `resolve_task_vendors` still raises on same-vendor (`same-vendor Codex review was not accepted` / `same-vendor Claude review was not accepted`) and the default may still resolve to `(claude, codex)` because the reviewer fallback hasn't been updated. This is RED.

- [ ] **Step 3: Update `resolve_task_vendors` — reviewer precedence + drop the same-vendor raise**

Replace `resolve_task_vendors` (lines ~231-256) body from the `reviewer = requested_reviewer or {...}[writer]` block onward:

```python
    writer = requested_writer or config.get("default_writer")
    if writer == "fake" and allow_test_adapter:
        reviewer = requested_reviewer or "fake"
        if reviewer != "fake":
            raise HarnessError("the internal fake writer must use the internal fake reviewer")
        return "fake", "fake"
    if writer not in REAL_MODEL_VENDORS:
        raise HarnessError("harness config default_writer must be codex or claude")
    reviewer = (
        requested_reviewer
        or config.get("default_reviewer")
        or {"codex": "claude", "claude": "codex"}[writer]
    )
    if reviewer not in REAL_MODEL_VENDORS:
        raise HarnessError("reviewer must be codex or claude; fake is selftest-only")
    return writer, reviewer
```

(The only removals vs. the original: the trailing `if reviewer == writer: raise` is gone, and the reviewer now prefers `config["default_reviewer"]` before the opposite-vendor fallback.)

- [ ] **Step 4: Rename `validate_vendor_separation`→`validate_model_vendors` and drop its same-vendor raise**

Replace the function (lines ~259-269):

```python
def validate_model_vendors(
    contract: Mapping[str, Any], *, allow_test_adapter: bool = False
) -> None:
    writer = contract.get("writer")
    reviewer = contract.get("reviewer")
    if allow_test_adapter and writer == reviewer == "fake":
        return
    if writer not in REAL_MODEL_VENDORS or reviewer not in REAL_MODEL_VENDORS:
        raise HarnessError("production tasks may use only codex and claude model vendors")
```

Update BOTH call sites (`task_runner.py:2746` and `:3177`) from `validate_vendor_separation(contract, ...)` to `validate_model_vendors(contract, ...)`.

- [ ] **Step 5: Run the selftest to verify it passes**

Run: `scripts/agent-harness selftest --ci`
Expected: `agent harness selftest: PASS`. Same-vendor resolves; default is `(claude, claude)`; `--agent codex` alone yields `(codex, claude)` (from `default_reviewer`); public `fake` still rejected; internal `fake→fake` (with `allow_test_adapter`) still works.

- [ ] **Step 6: Commit**

```bash
git add .agents/harness/config.json .agents/harness/task_runner.py
git commit -m "feat(harness): allow any writer/reviewer vendor pair; default claude->claude"
```

---

## Task 2: Relax `hook_guard._validate_provenance` for same-vendor

**Files:**
- Modify: `.agents/harness/hook_guard.py` (`_validate_provenance` ~708-726; selftest — add a direct provenance unit assertion)

**Interfaces:**
- Consumes: `_validate_provenance(task, writer, reviewer, *, allow_test_adapter=False)` (read the exact signature in the file). No new exports.

- [ ] **Step 1: Add a failing selftest assertion for same-vendor provenance**

Read `_validate_provenance` (starts at `.agents/harness/hook_guard.py:708`) to learn its exact parameters (it takes the task dict plus writer/reviewer identity dicts). In `_run_selftest`, near the other provenance/finish-guard assertions, add a direct unit check that a same-vendor **real** pair passes the vendor rule while a public `fake` pair is rejected. Construct minimal inputs matching the real signature, e.g.:

```python
        # Same-vendor real pairs pass the provenance vendor rule (session
        # independence is enforced separately). Public fake pairs are rejected.
        same_vendor_task = {"writer": "claude", "reviewer": "claude"}
        ok_identity_w = {"vendor": "claude", "cli_version": "x", "model": "m", "session_id": "sess-w"}
        ok_identity_r = {"vendor": "claude", "cli_version": "x", "model": "m", "session_id": "sess-r"}
        try:
            _validate_provenance(same_vendor_task, ok_identity_w, ok_identity_r)
            test.result("same-vendor provenance accepted", "ACCEPT", "ACCEPT")
        except GuardFailure as exc:
            test.result("same-vendor provenance accepted", f"BLOCK:{exc}", "ACCEPT")
        fake_task = {"writer": "fake", "reviewer": "fake"}
        try:
            _validate_provenance(fake_task, {"vendor": "fake"}, {"vendor": "fake"})
            test.result("public fake provenance rejected", "ACCEPT", "BLOCK")
        except GuardFailure:
            test.result("public fake provenance rejected", "BLOCK", "BLOCK")
```

Adjust the identity-dict fields to whatever `_validate_provenance` actually reads (it checks `writer.get("vendor")` against `task["writer"]`, and session/cli/model fields — match them so the ONLY thing under test is the vendor-equality rule). If the function needs more fields to reach the vendor check, add them; the goal is a non-vacuous assertion isolating the same-vendor rule.

Run: `bash .claude/hooks/selftest.sh`
Expected: FAIL — `same-vendor provenance accepted: got BLOCK:writer and reviewer must use different vendors, want ACCEPT`. RED.

- [ ] **Step 2: Delete the same-vendor rejection in `_validate_provenance`**

Remove these two lines (currently `hook_guard.py:722-723`):

```python
    elif writer_vendor == reviewer_vendor:
        raise GuardFailure("writer and reviewer must use different vendors")
```

Keep the preceding `allow_test_adapter` fake branch and the `writer_vendor not in {"codex","claude"} or reviewer_vendor not in {"codex","claude"}` rejection, and the following `writer.get("vendor") != task.get("writer")` / reviewer identity-match checks. (After removal, the `if/elif` becomes an `if allow_test_adapter …: … elif … not in {...}: raise`.)

- [ ] **Step 3: Run the hook selftest to verify it passes**

Run: `bash .claude/hooks/selftest.sh`
Expected: `guardrail self-test: PASS`. The new `same-vendor provenance accepted` = ACCEPT and `public fake provenance rejected` = BLOCK; the existing `reviewer reused writer session` case still BLOCK (session independence intact); all prior assertions unchanged.

- [ ] **Step 4: Commit**

```bash
git add .agents/harness/hook_guard.py
git commit -m "feat(harness): finish-guard accepts same-vendor pairs (session independence still enforced)"
```

---

## Task 3: Relax the `config_audit` default-writer pin

**Files:**
- Modify: `.agents/harness/config_audit.py` (the `default_writer == "claude"` invariant ~line 129-133)

- [ ] **Step 1: Run config-audit to confirm it currently rejects the new config**

Run: `scripts/agent-config-audit --ci`
Expected: with `default_reviewer: "claude"` already added (Task 1) and `default_writer: "claude"`, the existing invariant `default_writer == "claude"` still passes — so this step is a no-op check. To make the audit MEANINGFULLY validate the new field, proceed to Step 2. (If you want a RED first: temporarily set `default_writer` to `"codex"` in config, run the audit, watch it fail on the old pin, then revert.)

- [ ] **Step 2: Replace the invariant**

Replace the `default_writer` check (currently ~`config_audit.py:129-133`):

```python
    audit.require(
        isinstance(config, dict) and config.get("default_writer") == "claude",
        "harness default_writer is claude",
        "harness default_writer must be claude (Codex is the automatic reviewer)",
    )
```

with validation that both defaults are real vendors:

```python
    audit.require(
        isinstance(config, dict) and config.get("default_writer") in ("codex", "claude"),
        "harness default_writer is a real vendor",
        "harness default_writer must be codex or claude",
    )
    audit.require(
        isinstance(config, dict) and config.get("default_reviewer") in ("codex", "claude"),
        "harness default_reviewer is a real vendor",
        "harness default_reviewer must be codex or claude",
    )
```

- [ ] **Step 3: Run config-audit to verify green**

Run: `scripts/agent-config-audit --ci`
Expected: `agent config audit: PASS (N checks, 0 warnings)` — N increases by 1 (the added `default_reviewer` check). The `harness-config` fingerprint line changes (expected, informational).

- [ ] **Step 4: Commit**

```bash
git add .agents/harness/config_audit.py
git commit -m "chore(harness): config-audit validates default_writer + default_reviewer are real vendors"
```

---

## Task 4: Docs — skill, AGENTS.md, binding rule (both vendors)

**Files:**
- Modify: `.claude/skills/harness/SKILL.md`, `.agents/skills/harness/SKILL.md` (keep byte-identical)
- Modify: `AGENTS.md`
- Modify: `.claude/rules/agentic-workflow.md`, `.codex/rules/agentic-workflow.md` (keep byte-identical)

- [ ] **Step 1: Update the `/harness` skill (both mirrors)**

In `.agents/skills/harness/SKILL.md`, add a "Choosing the writer/reviewer pair" subsection near the "Run it" block:

```markdown
## Choosing the writer/reviewer pair

Pick vendors per task with `init`:

    scripts/agent-harness init <task-id> --agent <codex|claude> --reviewer <codex|claude> --prompt "…" --owned <path>

- `--agent` = writer vendor (default: `config.json` `default_writer`).
- `--reviewer` = reviewer vendor (default: `config.json` `default_reviewer`).
- All four pairs are allowed: `claude→codex`, `codex→claude`, `claude→claude`, `codex→codex`.
- **Default is `claude→claude`** — the harness runs entirely on Claude, no Codex dependency.
- The reviewer is ALWAYS a fresh, independent session with no writer context, whatever the pair — so same-vendor is still a real adversarial review, not self-grading.
- **Trade-off:** same-vendor loses cross-model-family diversity (Codex and Claude catch different failure modes). Prefer a cross-vendor pair when that diversity matters most (e.g. lock/crypto/egress changes).
```

Then `cp .agents/skills/harness/SKILL.md .claude/skills/harness/SKILL.md` and confirm `diff` is empty.

- [ ] **Step 2: Update `AGENTS.md`**

In the "Opt-in harness (/harness)" section, update the guard-behavior/vendor line to state: any of the four writer/reviewer pairs is selectable via `--agent`/`--reviewer`; the default is `claude→claude`; the reviewer is always a fresh independent session. Do not introduce the banned semantic-lint strings (`angular 18`, `allowSignalWrites`, `provideExperimentalZonelessChangeDetection`).

- [ ] **Step 3: Update the binding rule (both mirrors, byte-identical)**

In `.claude/rules/agentic-workflow.md`, find the text stating the Claude→Codex default and "same-vendor production review … forbidden" and replace it with:

```markdown
The writer/reviewer pair is configurable per task (`--agent` / `--reviewer`): any of `claude→codex`, `codex→claude`, `claude→claude`, `codex→codex`. The default is **`claude→claude`**. Whatever the pair, the reviewer is ALWAYS a fresh, independent session with no writer context — the implementer never owns the verdict. Prefer a cross-vendor pair when model-family diversity matters (lock/crypto/egress). The selftest-only `fake` adapter stays forbidden in production.
```

Then copy the same change into `.codex/rules/agentic-workflow.md` so the two files stay byte-identical (`diff` empty). If any surrounding line already differs between the two vendor copies, preserve that pre-existing difference and only mirror THIS edit.

- [ ] **Step 4: Verify config-audit stays green (rule parity + semantic lint)**

Run: `scripts/agent-config-audit --ci`
Expected: PASS. Rule parity holds (both `agentic-workflow.md` byte-identical); no banned semantic-lint strings introduced. Also run `diff .agents/skills/harness/SKILL.md .claude/skills/harness/SKILL.md` (empty) and `diff .claude/rules/agentic-workflow.md .codex/rules/agentic-workflow.md` (empty).

- [ ] **Step 5: Commit**

```bash
git add .claude/skills/harness/SKILL.md .agents/skills/harness/SKILL.md AGENTS.md .claude/rules/agentic-workflow.md .codex/rules/agentic-workflow.md
git commit -m "docs(harness): document configurable writer/reviewer pairs + claude->claude default"
```

---

## Task 5: Full-gate verification

**Files:** none (verification).

- [ ] **Step 1: All three gates green**

Run:
```bash
scripts/agent-harness selftest --ci
bash .claude/hooks/selftest.sh
scripts/agent-config-audit --ci
```
Expected: `agent harness selftest: PASS`; `guardrail self-test: PASS (N assertions)`; `agent config audit: PASS`.

- [ ] **Step 2: Confirm the invariants were preserved (read-only check)**

Grep to confirm the session-independence checks are intact and untouched by this change:
```bash
git diff murmur...HEAD -- .agents/harness/hook_guard.py | grep -E "session_id|reused|unique" || echo "session-independence code NOT in the diff (untouched — correct)"
```
Expected: the session-independence logic is NOT part of this diff (only the vendor-equality rule was removed).

- [ ] **Step 3: Open PR (do not merge until CI green)**

```bash
git push -u origin feat/harness-vendor-pairing
gh pr create -R murmur-io/murmur --base murmur --title "Configurable harness writer/reviewer vendor pairing (default claude->claude)"
```

---

## Self-review notes

- **Spec §3.1** (config + resolve) → Task 1. **§3.2** (3 relaxations) → Task 1 (resolve, validate rename) + Task 2 (provenance). **§3.3** (config_audit) → Task 3. **§3.4** (preserved invariants) → verified in Task 5 Step 2 + not touched anywhere. **§3.5** (docs) → Task 4. **§4** (testing) → each task's selftest steps. **§5** (rollout) → Task 5 Step 3.
- **Reviewer precedence** is defined once (Global Constraints) and implemented in Task 1 Step 3; the `--agent codex` test (Task 1 Step 1) asserts the `default_reviewer` fallback yields `(codex, claude)`.
- **Type consistency:** `validate_model_vendors` defined in Task 1 Step 4, both call sites updated in the same step. `default_reviewer` added in Task 1 Step 2, consumed by `resolve_task_vendors` (Task 1 Step 3) and `config_audit` (Task 3).
- **No placeholder:** every code step shows the exact edit; the one read-first step (Task 2 Step 1, `_validate_provenance` signature) is explicit about matching the real parameters.
