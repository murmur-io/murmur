#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# secret-scan.sh — PreToolUse(Bash) secret gate on `git commit`.
#
# WHY: Murmur is a privacy product with BYO API keys, dev DEK/KEK escape hatches,
# and Developer-ID signing material. One regex gate on the staged diff deletes an
# entire leak class before it ever reaches history. Scans ADDED lines only.
#
# CONTRACT: reads PreToolUse hook JSON on stdin.  exit 0 = allow, exit 2 = BLOCK.
# Only fires when the command is a `git commit`. Lockfiles (sha256 checksums are
# 64-hex → false positives) are excluded. Documented dev hatches are allowlisted.
# Override a genuine false positive by prefixing the commit: MURMUR_ALLOW_SECRET=1
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)"
[ -z "$cmd" ] && exit 0

# Gate git commits only.
printf '%s' "$cmd" | grep -Eq '\bgit[[:space:]]+commit\b' || exit 0
[ "${MURMUR_ALLOW_SECRET:-0}" = "1" ] && exit 0

# Staged ADDITIONS only. Exclusions:
#   - lockfiles: their sha256 checksums are 64-hex → false positives.
#   - .claude/hooks/: the guardrail scripts + selftest fixtures necessarily contain
#     secret-SHAPED regexes/strings (sk-ant-…, a fake 64-hex) to detect and test the
#     real thing. These are tiny, reviewed shell files — the same known-false-positive
#     carve-out we make for lockfiles.
diff="$(git diff --cached --no-color -U0 2>/dev/null \
          -- . ':(exclude)*.lock' ':(exclude)Cargo.lock' ':(exclude)package-lock.json' ':(exclude)*-lock.json' \
               ':(exclude).claude/hooks/*' \
        | grep -E '^\+' | grep -Ev '^\+\+\+' || true)"
[ -z "$diff" ] && exit 0

hits=""
add() { hits="${hits}
  • $1"; }

# High-signal, near-zero false positive → always block.
printf '%s' "$diff" | grep -Eq -- '-----BEGIN [A-Z ]*PRIVATE KEY-----'      && add "PEM private key block"
printf '%s' "$diff" | grep -Eq 'sk-ant-[A-Za-z0-9_-]{20,}'                  && add "Anthropic API key (sk-ant-…)"
printf '%s' "$diff" | grep -Eq 'ghp_[A-Za-z0-9]{36}'                        && add "GitHub personal access token (ghp_…)"
printf '%s' "$diff" | grep -Eq 'gho_[A-Za-z0-9]{36}'                        && add "GitHub OAuth token (gho_…)"
printf '%s' "$diff" | grep -Eq 'github_pat_[A-Za-z0-9_]{22,}'               && add "GitHub fine-grained PAT (github_pat_…)"
printf '%s' "$diff" | grep -Eq 'AKIA[0-9A-Z]{16}'                           && add "AWS access key id (AKIA…)"
printf '%s' "$diff" | grep -Eq 'xox[baprs]-[A-Za-z0-9-]{10,}'               && add "Slack token (xox…)"
printf '%s' "$diff" | grep -Eq 'sk-[A-Za-z0-9]{32,}'                        && add "OpenAI-style secret key (sk-…)"

# 64-hex DEK/KEK material. Skip checksum/hash contexts and the documented dev hatches
# (MURMUR_DEV_DEK/KEK lines + the fixed 0123…ef dev key that lives in CLAUDE.md by design).
hex64="$(printf '%s' "$diff" \
          | grep -Eiv 'MURMUR_DEV_DEK|MURMUR_DEV_KEK|checksum|integrity|sha-?(256|512)|blake' \
          | grep -Eio '[0-9a-f]{64}' \
          | grep -Eiv '^(0123456789abcdef){4}$' \
          | head -1 || true)"
[ -n "$hex64" ] && add "64-hex secret (DEK/KEK-shaped): ${hex64:0:12}…"

if [ -n "$hits" ]; then
  {
    echo "🛑 secret-scan blocked the commit — the staged diff looks like it contains secret material:"
    printf '%s\n' "$hits"
    echo "   Secrets belong in the macOS Keychain, never in git history."
    echo "   False positive? Re-run the commit prefixed with MURMUR_ALLOW_SECRET=1."
  } >&2
  exit 2
fi
exit 0
