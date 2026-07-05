#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# autoformat.sh — PostToolUse(Edit|Write|MultiEdit) opt-in formatter.
#
# Default OFF (MURMUR_AUTOFMT unset or 0) so it never surprises a concurrent
# session with churn or fights the repo's own formatting. Enable with
# MURMUR_AUTOFMT=1 in the environment.
#
# Only rustfmt on .rs (fast, standard, matches src-tauri). Frontend formatting
# deliberately stays with `npx ng lint` (angular-zoneless rules: dir-per-component ts/html/scss,
# strict per-component style budget) — a blind prettier pass would fight those.
# Never blocks: always exit 0.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
[ "${MURMUR_AUTOFMT:-0}" = "1" ] || exit 0
input="$(cat)"
f="$(printf '%s' "$input" | jq -r '.tool_input.file_path // empty' 2>/dev/null)"
{ [ -z "$f" ] || [ ! -f "$f" ]; } && exit 0
case "$f" in
  *.rs) command -v rustfmt >/dev/null 2>&1 && rustfmt "$f" >/dev/null 2>&1 || true ;;
esac
exit 0
