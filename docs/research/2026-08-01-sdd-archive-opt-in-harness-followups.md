<!-- Archived 2026-08-01 from the untracked `.superpowers/sdd/` ledger before that directory was removed. The SDD run itself (plan: docs/superpowers/plans/2026-07-24-opt-in-harness-skill.md, branch feat/opt-in-harness) shipped as PR #441; only these deferred items never found a home in tracked files. -->
# Archive — deferred findings from the opt-in-harness SDD run

The `.superpowers/sdd/` directory held a progress ledger and ten review diffs from the
opt-in-harness build (Tasks 1–7, all reviewed clean, verification green: selftest 232 PASS,
config-audit 136 PASS, harness selftest PASS). The work merged; the ledger did not, and it was
untracked, so deleting the directory would have silently dropped the items below. They are
recorded here verbatim in substance, with current-tree verification where it was cheap.

## 1. Task 6 — resolved after the ledger was written

The ledger lists Task 6 (`release-murmur` recipe + `scripts/release.sh` deprecation) as *pending*.
**Verified 2026-08-01: it is done, on both halves.** The recipe lives in the `release-murmur` skill,
which declares itself authoritative and supersedes `docs/RELEASE-CHECKLIST.md`; and
`scripts/release.sh` already carries the deprecation header ("DEPRECATED — smoke test only, NOT the
release path"), naming the stale `MeetNotes.app` bundle and pointing at the skill plus
`scripts/macos-sign-notarize.sh`.

Recorded because a stale "pending" is worse than no note: it invites someone to redo finished work.

One standing caution the ledger did NOT make explicit — the plan called for a deprecation **header**,
never deletion. `scripts/release.sh` is referenced by `.agents/harness/resource_policy.py` (its
heavy-command list), `docs/STATUS.md`, `docs/RELEASE-CHECKLIST.md`, and both copies of the
`release-murmur` skill. Deleting the file would silently change resource-lane policy.

## 2. Deferred security finding — `hook_guard.py` `_resolve_task`

A glob-discovered task manifest containing **unparseable JSON**, when it is the only manifest
for the worktree, falls through to `NoTaskForWorktree` and therefore **relaxes both guard
layers**, while the spec says a malformed manifest must BLOCK.

Containment as assessed at the time: both layers stay consistent (both relax), and every
dangerous invalid case still fails closed — explicit-id malformed, ambiguous manifests, wrong
worktree, and failed attestation all BLOCK. The fix is genuinely subtle: without parsing, the
guard cannot tell whose corrupt manifest it found, so it cannot attribute the failure to this
worktree.

This is the one item here worth turning into an oracle rather than prose: a guard selftest
asserting `malformed sole manifest → BLOCK` would make the intended behavior falsifiable.

## 3. Minor findings, follow-up only

- **`hook_guard.py`** — the anti-bypass non-vacuousness safe-control check existed only as a
  throwaway script, never a persisted assertion. Adjacent pre-existing LIGHT assertions around
  `command_is_heavy` cover the same logic.
- **`hook_guard.py`** — advisory-mode no-task returns `None` silently with no breadcrumb.
  Intentional for opt-in: printing on every normal commit would be noise. No fix wanted.
- **`resource_policy.py`** — `_has_live_substitution` flags `<(` / `>(` process substitution as
  live, but the heavy-command recursion only inspects backtick and `$()` bodies, so
  `gh ... <(cargo build)` is not caught by `command_is_heavy`. Pre-existing (the base commit has
  it too), and the block-bash indirection guard already blocks `<(` outside quotes.
- **`hook_guard.py`** — the unit assertions added by that run call `command_is_heavy` directly
  rather than through the block-bash subprocess path (`command_is_heavy_in` is a thin
  pass-through). Low risk.
- **`hook_guard.py`** — three of those assertions sit inside the per-vendor loop and therefore
  execute twice each. Cosmetic.
