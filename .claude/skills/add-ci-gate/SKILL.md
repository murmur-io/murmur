---
name: add-ci-gate
description: The discipline for adding or changing a check in Murmur's CI gate the right way — put it in scripts/ci.sh (the single source of truth), in the correct order, tool-absent-safe, without slowing the iterate loop, and (for a guardrail hook) with a RED-before-GREEN selftest assertion; then confirm GitHub Actions still just wraps ci.sh. Use whenever the user wants to add a lint/test/security/format/build check to CI, tighten the gate, or wire a new tool into the pipeline.
---

# /add-ci-gate — add a CI check without breaking the gate

Adding a check to Murmur's CI is not "append a line to the YAML". The gate has one source of truth and a specific shape. Do it in this order.

## The rule that prevents drift

**Add the check to `scripts/ci.sh`, NOT to `.github/workflows/ci.yml`.** The workflow calls `bash scripts/ci.sh`, so a step added to ci.sh runs in CI for free — with zero risk of the local gate and the cloud gate disagreeing. You touch `ci.yml` ONLY when the new step needs a **runner-level prerequisite** (a `brew install <tool>`, a `taiki-e/install-action` for a prebuilt cargo tool, a cache, a toolchain component) that the local Mac already has.

## Step 1 — decide WHERE in ci.sh (order is load-bearing)

`ci.sh` is `set -euo pipefail` — the first non-zero exit stops the run. So order = fail-fast + cheap-before-expensive:

1. guardrail selftest (instant)
2. cheap static checks (swiftc typecheck, clippy, tests)
3. **supply-chain BEFORE the build** (`cargo audit`, `cargo deny`) — catch a bad dependency before paying for a full compile
4. builds (`cargo build`, `ng build`)
5. heavy E2E last (behind `MURMUR_CI_SKIP_E2E`)

Put a fast check high (so a trivial failure fails in seconds); put anything slow/expensive as late as its dependencies allow.

## Step 2 — write the step tool-absent-safe

Match the existing style: a `── <name> ──` banner, then the command. If the check depends on a tool that may be missing on some host, degrade the way ci.sh already does — either skip-with-a-note (like `swiftc`) or self-install once (like `cargo-audit`/`cargo-deny`):

```bash
echo "── <your check> ──"
if command -v <tool> >/dev/null 2>&1; then
  <the check>            # non-zero exit = gate fails (set -e)
else
  echo "  (<tool> not found — skipping)"     # OR: install it, if it's cheap + deterministic
fi
```

Never let the step *pass silently when it didn't actually run* on the host that matters — a skip on a contributor Mac is fine, a skip on the CI runner that hides a real failure is a green-wash.

## Step 3 — don't slow the INNER loop

The gate is `ci.sh`; the agent inner loop is `scripts/agent-resource-run --chdir src-tauri -- cargo test --lib`. Every agent Cargo/rustc/full-CI command uses the wrapper. Adding to `ci.sh` is fine even if it is slowish; do not wire an expensive check into anything that runs every edit.

## Step 4 — if it's a GUARDRAIL HOOK, prove it with RED-before-GREEN

A new `.claude/hooks/*` guardrail (a block-bash pattern, a secret-scan rule) MUST get a **BLOCK and an ALLOW** assertion in `.claude/hooks/selftest.sh` (which ci.sh runs first). This is the "test the workflow itself" pattern — prove it blocks the bad case AND lets the good case through, so the guardrail can never silently go phantom. Keep assertions host-independent (string-based / throwaway-repo, like the existing ones) so they pass in CI too.

## Step 5 — wire the runner prerequisite (only if needed)

If the step needs something the CI runner lacks, add the minimal step to `.github/workflows/ci.yml`, following `/github-actions`:
- a prebuilt cargo tool → `taiki-e/install-action` (SHA-pinned), not `cargo install` (slow)
- a system tool → `brew list <t> >/dev/null 2>&1 || brew install <t>`
- a big download the step needs → an `actions/cache` step (like the whisper model)

CI runs the COMPLETE ci.sh on every PR (release parity, 2026-07-05), so placement no longer changes CI coverage — but it still changes the LOCAL iterating subset: a check before the `MURMUR_CI_SKIP_E2E` guard runs even in the fast local subset; an E2E-heavy check belongs inside the guarded `else` branch so `MURMUR_CI_SKIP_E2E=1` stays quick.

## Step 6 — verify (RED-before-GREEN for the gate too)

```bash
# 1) The new step FAILS when it should (introduce a temporary violation, confirm red), then remove it.
# 2) The gate is green with the real tree, via the exact CI command:
MURMUR_CI_SKIP_E2E=1 scripts/agent-resource-run -- bash scripts/ci.sh
scripts/agent-resource-run -- bash scripts/ci.sh
# 3) Guardrail added? prove it:
bash .claude/hooks/selftest.sh              # PASS
# 4) Touched the workflow? parse it:
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml')); print('yaml ok')"
command -v actionlint >/dev/null && actionlint .github/workflows/ci.yml || echo "(actionlint not installed)"
```

## Checklist

- [ ] Check lives in `scripts/ci.sh`, in the right fail-fast slot (supply-chain before build; heavy behind `MURMUR_CI_SKIP_E2E`).
- [ ] Tool-absent-safe (skip-with-note or deterministic self-install) — never a silent false-pass on the host that matters.
- [ ] Didn't add anything expensive to the inner loop; no bare `clippy --all-targets`.
- [ ] Guardrail hook? BLOCK + ALLOW assertion added to `selftest.sh`, host-independent.
- [ ] Runner prerequisite (if any) added to `ci.yml` per `/github-actions`; workflow still just `bash scripts/ci.sh`.
- [ ] RED-before-GREEN proven; full/subset `ci.sh` green; no new dep added without user approval.
- [ ] Landed via a PR into `murmur` (never direct-push), JakubGawr author, no Claude trailers.
