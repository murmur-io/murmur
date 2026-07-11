#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# block-bash.sh — PreToolUse(Bash) deterministic guardrail for Murmur.
#
# WHY THIS EXISTS: prose rules in AGENTS.md/.codex/rules are advisory — they
# degrade under context pressure and the agent can talk itself out of them. This
# hook is the one layer it cannot. Every block below encodes an incident that has
# ALREADY cost the project (see docs/research/2026-07-02-claude-setup-audit.md and
# the "hard-won release rules" in AGENTS.md).
#
# CONTRACT: reads the PreToolUse hook JSON on stdin, extracts .tool_input.command.
#   exit 0            → allow
#   exit 2 + stderr   → BLOCK; stderr is surfaced to the model as the reason.
# The hook pattern-matches the command STRING; it does not try to understand intent.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)"
[ -z "$cmd" ] && exit 0
is_codex_payload=0
printf '%s' "$input" | jq -e 'has("hook_event_name") or has("session_id") or has("tool_name")' >/dev/null 2>&1 && is_codex_payload=1

# Collapse newlines so multi-line commands match on one line.
norm="$(printf '%s' "$cmd" | tr '\n' ' ')"

deny() {
  if [ "$is_codex_payload" = "1" ]; then
    jq -cn --arg reason "block-bash refused this command: $1 $2" '{decision:"block",reason:$reason}'
    exit 0
  fi
  echo "🛑 block-bash refused this command:" >&2
  echo "   $1" >&2
  echo "   → $2" >&2
  exit 2
}

# 1) Catastrophic recursive deletes of / or $HOME. (Standard, near-zero false positive.)
if printf '%s' "$norm" | grep -Eq '\brm\b([[:space:]]+-[a-zA-Z]*[rR][a-zA-Z]*|[[:space:]]+-[a-zA-Z]*f[a-zA-Z]*)' \
   && printf '%s' "$norm" | grep -Eq '[[:space:]](/|~|/\*|\$HOME|\$\{HOME\}|~/\*)([[:space:]]|$)'; then
  deny "recursive delete targeting / or \$HOME." \
       "scope the delete to a project path; refusing to nuke the root/home tree."
fi

# 2) Direct push to the protected trunk. Trunk = murmur (local main tracks origin/murmur).
#    PR-merge is the ONLY sanctioned path (AGENTS.md release rule 6). Feature-branch
#    pushes and PRs are allowed — only pushes that LAND ON murmur/main/master are blocked.
if printf '%s' "$norm" | grep -Eq '\bgit[[:space:]]+push\b'; then
  # 2a) explicit protected destination: `git push origin murmur`, `... HEAD:main`, `...:master`
  if printf '%s' "$norm" | grep -Eq '\bgit[[:space:]]+push\b[^;&|]*[[:space:]](murmur|main|master)([[:space:]]|$)' \
     || printf '%s' "$norm" | grep -Eq ':(murmur|main|master)([[:space:]]|$)'; then
    deny "direct push to the protected trunk (murmur/main/master)." \
         "land via a PR: gh pr create --base murmur … then gh pr merge … --merge."
  fi
  # 2b) bare `git push` while HEAD itself is a protected branch
  head_branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo '')"
  if printf '%s' "$norm" | grep -Eq '\bgit[[:space:]]+push\b[[:space:]]*(--[a-z-]+[[:space:]]*)*$' \
     && printf '%s' "$head_branch" | grep -Eq '^(murmur|main|master)$'; then
    deny "bare 'git push' while on protected branch '$head_branch'." \
         "check out a feature branch and open a PR against murmur."
  fi
fi

# 3) The macOS `security` / keychain CLI. It blocks on the auth dialog the agent shell
#    can't surface → the command HANGS → retries queue → runaway procs (the 2026-06-27
#    11-hung-`security`-procs incident, AGENTS.md release rule 3). Process management
#    (pkill/pgrep/kill security) is fine — it never touches the keychain.
if printf '%s' "$norm" | grep -Eq '(^|[^-[:alnum:]/])security[[:space:]]+(unlock-keychain|lock-keychain|find-identity|find-generic-password|add-generic-password|delete-generic-password|find-certificate|import|export|set-keychain-settings|list-keychains|default-keychain|create-keychain|cms|set-key-partition-list)'; then
  deny "the macOS 'security'/keychain CLI hangs on the auth dialog and spawns runaway procs." \
       "run keychain ops yourself via '!' in the terminal; pkill/pgrep security are allowed."
fi
# 3b) notarytool store-credentials is the same interactive-auth trap.
if printf '%s' "$norm" | grep -Eq 'notarytool[[:space:]]+store-credentials'; then
  deny "'notarytool store-credentials' needs interactive auth the agent shell can't surface." \
       "store the 'murmur' notarytool profile yourself; the agent uses --keychain-profile murmur."
fi

# 4) `cargo clippy --all-targets` thrashes the openssl/sqlcipher build profile and times
#    out (rust-tauri rule §9). The inner loop is `cargo test --lib`; the full clippy gate
#    lives in scripts/ci.sh, which is invoked as `bash scripts/ci.sh` (not matched here).
if printf '%s' "$norm" | grep -Eq '\bcargo[[:space:]]+clippy\b' \
   && printf '%s' "$norm" | grep -Eq -- '--all-targets\b'; then
  deny "'cargo clippy --all-targets' thrashes the openssl/sqlcipher profile and times out." \
       "inner loop = (cd src-tauri && cargo test --lib); full gate = bash scripts/ci.sh (run once)."
fi

# 5) `codesign --deep` does NOT sign the nested Resources/ audio helpers → notarization
#    comes back Invalid (AGENTS.md release rule 2, the 2026-06-27 v0.4.0 failure).
if printf '%s' "$norm" | grep -Eq '\bcodesign\b' \
   && printf '%s' "$norm" | grep -Eq -- '--deep\b'; then
  deny "'codesign --deep' skips nested Resources/ helpers → notarization Invalid." \
       "sign inside-out; use scripts/macos-sign-notarize.sh (globs every helper)."
fi

exit 0
