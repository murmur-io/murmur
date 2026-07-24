# Opt-in harness (`/harness`) + relaxed normal mode — design

- **Date:** 2026-07-24
- **Status:** Approved (design); implementation plan pending
- **Author:** QueaT
- **Topic:** Make the agent development harness *opt-in* instead of *mandatory on every commit*, and relax the day-to-day shell guardrails that bit the last release — without weakening the two catastrophe-prevention rails (secret-scan, trunk protection).

---

## 1. Problem

The harness (`.agents/harness/` + the `.claude`/`.codex` adapters) is well-built and every rule has a real production incident behind it. Its one structural flaw is **lack of proportionality**: a docs typo pays the exact same ceremony as a change to `crypto.rs`. Two distinct layers of friction fall out of this, and they had been conflated:

**Layer 1 — commit/review ceremony (`finish-guard` + task machinery).** Every `git commit` the agent makes fails closed unless the current worktree has a harness task with a hash-bound PASS attestation (`hook_guard.py:923` `_finish_guard`; `MURMUR_FINISH_GUARD=enforce` pinned in `.claude/settings.json:4`). There is no light path — `kind:"docs"` runs the identical writer → checks → spec+adversarial review → attestation flow.

**Layer 2 — shell guardrails (`block-bash` + resource-lane + Claude Code sandbox).** Always-on, independent of harness mode. This is what actually hurt the last release.

Concrete pains observed on the last release (the failure surface this design must close):

| Pain | Root layer |
| --- | --- |
| `finish-guard` blocked a chore/version-bump commit → had to hack `MURMUR_FINISH_GUARD=advisory` | Layer 1 (finish-guard) |
| Claude Code sandbox blocked build / keychain / `gh` entirely | Layer 2 (sandbox) |
| Had to wrap every heavy command in `agent-resource-run` | Layer 2 (resource-lane) |
| `--body-file` needed because a PR body containing "cargo"/"tauri" was classified heavy | Layer 2 (`resource_policy` false positive) |
| Waiting on CI because repo auto-merge is off | GitHub setting (out of code) |

## 2. Decision

**Pure opt-in.** The harness becomes a tool the operator invokes deliberately (`/harness`), not a gate on every commit. This is an explicit, accepted trade-off (see §7): in normal mode, a commit touching risk-classified paths (`crypto`/lock/egress/…) is **not** auto-blocked. The safety net is *available* via `/harness`, not *forced*.

**One switch governs both layers.** The switch is the signal the harness already computes: *"does this worktree have an active harness task?"* (`_resolve_task`, `hook_guard.py:582`). No new persistent state, env flag, or lock file.

- **No task present (normal mode):** `finish-guard` sleeps; heavy commands run directly; only `secret-scan` + trunk protection fire.
- **Task present (harness mode, entered via `/harness`):** full ceremony + resource-lane serialization + attestation, exactly as today.

Because `/harness` creates an isolated sibling worktree (as `scripts/agent-harness init` does today), rigor physically lives in that worktree; the main `murmur` checkout never has a task, so day-to-day work there is unconstrained.

## 3. Design

### 3.1 The switch — guard changes (`.agents/harness/hook_guard.py`)

1. **`_finish_guard` (`hook_guard.py:923`): invert the no-task branch.** Today `_resolve_task` raising "no task manifest matches this worktree" → BLOCK. Change to: **no-task → ALLOW (return `None`)**. Keep BLOCK when a task *is present but invalid* (malformed manifest, branch mismatch, worktree mismatch, failed attestation) — the operator clearly intended rigor and something is wrong. `off` mode unchanged. `enforce` now means "enforce *if* a task is present."
   - Implementation note: distinguish the specific "no task at all" `GuardFailure` from the other `_resolve_task`/`_validate_attestation` failures so only the former is downgraded to ALLOW. Do **not** blanket-catch.
2. **Resource-lane heavy/dev block (`hook_guard.py:494-504`): gate on task-present.** No task → `command_is_heavy_in` / `command_is_dev_in` do not block (bare `cargo`, `tauri build`, `ng build`, `npm run dev` run directly). Task present → keep the current requirement to route through `scripts/agent-resource-run` / `scripts/agent-dev-run` (parallel worktrees must serialize on the shared cargo lane).
3. **`secret-scan` and trunk protection: unchanged, unconditional.** They fire in both modes (`_secret_scan` `hook_guard.py:539`; `_push_targets_protected` `hook_guard.py:411`, `PROTECTED_BRANCHES = {"murmur","main","master"}`).

### 3.2 block-bash fixes (always-on, both modes)

4. **Unblock `source ~/.cargo/env`.** The indirection guard (`_unsupported_execution_indirection`, `hook_guard.py:282`) hard-blocks `source`/`.` with no allowlist, even though `SAFE_SOURCE_TARGETS` already exists in `resource_policy.py:43` (it is only consulted by the heavy classifier, never by the indirection guard, which fires first). Make the indirection guard consult that allowlist for `source`/`.` targets. CLAUDE.md lists `source ~/.cargo/env` as a standard command; it must not be blocked.
5. **Stop classifying `gh` as heavy.** A `gh pr create --body "...cargo..."` was flagged because `resource_policy` fail-closes on commands that mention `cargo|rustc|tauri` when it cannot confidently parse them, and/or substring-matches quoted argument text. Add `gh` to the always-allowed set (like `grep`/`rg`, `READ_ONLY_SEARCHES` `resource_policy.py:15`) and ensure quoted-argument content is not substring-matched against command names. Removes the `--body-file` workaround.

### 3.3 The `/harness` entry (both vendors)

6. **Claude:** `.claude/skills/harness/SKILL.md` — a guided wrapper over `scripts/agent-harness init/run/commit/close`. Canonical runbook mirrored at `.agents/skills/harness/SKILL.md` per repo convention. The skill explains: when to reach for rigor, how the switch works, and that it operates in an isolated worktree.
7. **Codex:** Codex has **no skills mechanism** (verified: `.codex/` has `agents/`, `rules/`, `hooks/`, but no `skills/` or `prompts/`). Document the opt-in path in `AGENTS.md` ("want rigor → `scripts/agent-harness`"). Guard behavior is identical across vendors because both adapters call the same `hook_guard.py`, so behavioral parity is free.
8. **`config_audit` impact:** skills are *not* part of the parity contract (`config_audit.py` checks `rules/` and `agents/` for cross-vendor parity, not `skills/`). A Claude-only skill does not break `config_audit --ci`. Keep the `.agents/` ↔ `.claude/` mirror convention regardless.

### 3.4 Sandbox + release durability (docs, not sandbox weakening)

9. **Keep the project sandbox strict.** The `sandbox` block in `.claude/settings.json` (`enabled:true`, `failIfUnavailable:true`) stays. The per-machine `.claude/settings.local.json` (git-ignored) remains the release-machine override — do not commit sandbox-disabling to the repo (it would weaken the sandbox for the whole team).
10. **Bake the release routine into the `release-murmur` skill** so a smooth release does not depend on memory recall: the unsandboxed steps, the human pause points (P1 = unlock login keychain; P3 = approve the Developer-ID codesign key ACL dialog once; P2 = supply the 40-hex `DEVELOPER_ID` up front), and the correct order. Fix or deprecate the stale `scripts/release.sh` (it still targets `MeetNotes.app`; the real bundle is `Murmur.app` at the workspace-root `target/`).
11. **Emergent benefit:** with `finish-guard` asleep and the resource-lane not forcing wrappers in normal mode, the release becomes largely smooth without writing a new driver script — the version-bump commit goes through directly and heavy build commands run directly. A dedicated "one resumable command" release driver is therefore **out of scope** here (see §9); the pain it addressed is mostly removed by §3.1.

## 4. Fixes → pains (traceability)

| Pain | Resolved by |
| --- | --- |
| `finish-guard` blocked chore → `advisory` hack | §3.1.1 (normal mode: guard sleeps) |
| sandbox blocked build/keychain/gh | §3.4 (`settings.local.json` + recipe baked into `release-murmur`) |
| wrapping heavy commands in `agent-resource-run` | §3.1.2 (normal mode: heavy commands run directly) |
| `--body-file` due to "cargo" in PR body | §3.2.5 (`gh` exempt from heavy classification) |
| waiting on CI (auto-merge off) | Out of code — enable repo auto-merge; documented in `release-murmur` |

## 5. Components & boundaries

- **`hook_guard.py`** — the single canonical guard (both vendor adapters call it). All switch logic and block-bash fixes live here + `resource_policy.py`. No Claude↔Codex drift risk; `config_audit` only fingerprints `hook_guard.py` (informational) and does not inspect `resource_policy.py`.
- **`resource_policy.py`** — heavy/dev classifier. Receives the `gh` exemption and the quoted-arg fix. Its `command_is_heavy_in`/`command_is_dev_in` callers in `hook_guard.py` become task-gated.
- **`scripts/agent-harness`** — unchanged CLI; `/harness` is a thin guided entry to it.
- **`.claude/skills/harness/`, `.agents/skills/harness/`, `AGENTS.md`** — the opt-in entry, per vendor.
- **`.claude/skills/release-murmur/`** — gains the durable release recipe.

## 6. Testing

- **`hook_guard --selftest`** (the real gate for guard changes; `config_audit` does not cover this logic):
  - Invert the existing "default-enforce missing manifest → BLOCK" assertion (`hook_guard.py:~1807`) to **ALLOW**.
  - Add RED assertions (must still BLOCK): task present but invalid → BLOCK; staged secret in normal mode → BLOCK; `git push origin murmur` / bare push on `murmur` in normal mode → BLOCK; heavy command with a task present but unwrapped → BLOCK.
  - Add ALLOW assertions: heavy command with no task → ALLOW; `source ~/.cargo/env` → ALLOW; `gh pr create --body "...cargo..."` → ALLOW.
- **`config_audit --ci`** must stay green: `MURMUR_FINISH_GUARD` stays `enforce` in `settings.json` (semantics changed, wiring unchanged); no new wired hooks; no rule/agent parity changes.
- **`scripts/agent-config-audit`** and **`scripts/agent-harness selftest`** run clean.

## 7. Accepted risk / trade-offs

- **Explicit operator choice — pure opt-in.** In normal mode, a commit touching a risk-classified path (`config.json risk_classification`: `crypto.rs`, `secrets/**`, `storage/**`, lock/egress/protocol/etc.) is **not** auto-blocked and gets **no** review. The lock-security / egress-security review nets are available only when the operator runs `/harness`. This is knowingly weaker than the previous fail-closed-everywhere posture; it is acceptable because this is a solo repo and the operator opts into rigor deliberately. A future middle option ("risk-gated normal mode") is recorded in §9 but explicitly not built now.
- **Preserved regardless of mode:** secret-in-git-history prevention (`secret-scan`) and direct-push-to-trunk prevention (branch protection). In harness mode, the full contract is unchanged: hash-bound attestation, vendor separation (writer ≠ reviewer), staged-diff binding, required reviews + risk evidence.

## 8. Rollout / migration

- The change edits `hook_guard.py`, `resource_policy.py`, `.claude/`, `.agents/`, `AGENTS.md` — all under `config.json protected_paths`. It also changes `instructions_sha256` (these files are in `instruction_paths`), invalidating any in-flight task receipts. Land it either through the harness as a `--kind harness` change (with the one-time `seal-prepared` bootstrap) or by committing from a normal terminal (no hooks).
- **Cleanup:** prune the ~20 abandoned worktrees under `../.murmur-agent-tasks/` (`git worktree remove`) — their receipts are invalidated by the fingerprint change anyway. Includes the stale `opt-in-harness-quick-lane-v2..v4` / `harness-light-lane-v2..v7` experiments this design supersedes.

## 9. Out of scope / follow-ups

- **Risk-gated normal mode** (block only risk-classified commits in normal mode, allow the rest): a stronger-safety alternative to pure opt-in. Deliberately deferred per operator choice.
- **"One resumable command" release driver** with explicit P1/P3 pauses: mostly obviated by §3.1/§3.4; revisit only if release still feels manual after this lands.
- **Repo auto-merge**: a GitHub setting, not code; note it in `release-murmur`.
