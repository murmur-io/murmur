#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# trace-span.sh — append one JSONL span to .claude/tmp/<task>/trace.jsonl.
# Best-effort observability for a ship-feature run. NEVER blocks, always exit 0.
#
# Usage: .claude/lib/trace-span.sh <task> <phase> <gate> <verdict> [artifact] [durationMs]
#   e.g. .claude/lib/trace-span.sh murmur-hooks verify adversarial-verify PASS adversarial-verify.json 41200
#
# Later: `jq -s .` over trace.jsonl answers "how many verify iterations did this take?".
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
task="${1:-}"; phase="${2:-}"; gate="${3:-}"; verdict="${4:-}"; artifact="${5:-}"; dur="${6:-}"
[ -z "$task" ] && exit 0
dir=".claude/tmp/$task"; mkdir -p "$dir" 2>/dev/null || exit 0
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"
branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
jq -cn --arg ts "$ts" --arg task "$task" --arg phase "$phase" --arg gate "$gate" \
       --arg verdict "$verdict" --arg artifact "$artifact" --arg dur "$dur" --arg branch "$branch" \
   '{ts:$ts,task:$task,phase:$phase,gate:$gate,verdict:$verdict,artifact:$artifact,durationMs:$dur,branch:$branch}' \
   >> "$dir/trace.jsonl" 2>/dev/null || true
exit 0
