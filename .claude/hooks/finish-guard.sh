#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# finish-guard.sh — PreToolUse(Bash) Definition-of-Done gate on `git commit`.
#
# Murmur's DoD is binding (CLAUDE.md): the verifier owns the verdict, not the
# author. This turns that from prose into a machine check — a commit for an active
# task may not land until its gate verdicts are PASS.
#
# Gate files live in .claude/tmp/<task>/  (written by the adversarial-verifier and
# lock-security-reviewer per .claude/agents/*.md):
#   adversarial-verify.json   → {"verdict":"PASS"|"FAIL", ...}         (always required)
#   lock-security.json        → {"verdict":"PASS"|"FAIL", ...}         (required iff .lock-touched present)
#
# MODE (env MURMUR_FINISH_GUARD, default "advisory"):
#   advisory → warn on stderr, exit 0 (never blocks)
#   enforce  → exit 2 (blocks the commit) when a required gate is missing / not PASS
#
# When there is no active task dir it stays completely out of the way (exit 0).
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)"
[ -z "$cmd" ] && exit 0
printf '%s' "$cmd" | grep -Eq '\bgit[[:space:]]+commit\b' || exit 0

mode="${MURMUR_FINISH_GUARD:-advisory}"
tmp=".claude/tmp"

# Resolve the active task: explicit pointer file wins, else a branch-named dir.
task=""
[ -f "$tmp/.current-task" ] && task="$(tr -d '[:space:]' < "$tmp/.current-task" 2>/dev/null)"
if [ -z "$task" ]; then
  br="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo '')"
  [ -n "$br" ] && [ -d "$tmp/$br" ] && task="$br"
fi
{ [ -z "$task" ] || [ ! -d "$tmp/$task" ]; } && exit 0

dir="$tmp/$task"
missing=""
for gate in adversarial-verify lock-security; do
  f="$dir/$gate.json"
  if [ ! -f "$f" ]; then
    [ "$gate" = "lock-security" ] && [ ! -f "$dir/.lock-touched" ] && continue
    missing="${missing}
  • $gate.json is missing"
    continue
  fi
  v="$(jq -r '.verdict // "MISSING"' "$f" 2>/dev/null || echo MISSING)"
  [ "$v" = "PASS" ] || missing="${missing}
  • $gate.json verdict = $v (need PASS)"
done
[ -z "$missing" ] && exit 0

if [ "$mode" = "enforce" ]; then
  { echo "🛑 finish-guard: Definition-of-Done not met for task '$task':"; printf '%s\n' "$missing"
    echo "   The verifier owns the verdict — commit only after the gates are PASS."; } >&2
  exit 2
fi
{ echo "⚠️  finish-guard (advisory): DoD gaps for task '$task':"; printf '%s\n' "$missing"
  echo "   Set MURMUR_FINISH_GUARD=enforce to make this a hard block."; } >&2
exit 0
