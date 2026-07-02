#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# selftest.sh — proves the guardrail hooks actually block. Run standalone or from
# scripts/ci.sh. This is the meta-test that prevents another "phantom block-bash"
# (a documented guardrail that never existed): if a hook stops enforcing, CI goes red.
#
#   bash .claude/hooks/selftest.sh   → exit 0 all pass, exit 1 any assertion fails
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
HOOKS="$ROOT/.claude/hooks"
fail=0

as_json() { printf '{"tool_input":{"command":%s}}' "$(printf '%s' "$1" | jq -Rs .)"; }

expect() { # <label> <script> <command> <BLOCK|ALLOW>
  local label="$1" script="$2" cmd="$3" want="$4" rc got
  as_json "$cmd" | bash "$HOOKS/$script" >/dev/null 2>&1; rc=$?
  [ "$rc" -eq 2 ] && got=BLOCK || got=ALLOW
  if [ "$got" = "$want" ]; then printf '  ✅ %-34s %s\n' "$label" "$got"
  else printf '  ❌ %-34s got %s want %s\n' "$label" "$got" "$want"; fail=1; fi
}

echo "── block-bash.sh ──"
expect "push to trunk (murmur)"       block-bash.sh "git push origin murmur"                      BLOCK
expect "push HEAD:main"               block-bash.sh "git push origin HEAD:main"                    BLOCK
expect "force push master"            block-bash.sh "git push --force origin master"              BLOCK
expect "push a feature branch"        block-bash.sh "git push -u origin feat/x"                    ALLOW
expect "gh pr create --base murmur"   block-bash.sh "gh pr create --base murmur -t x"             ALLOW
expect "security unlock-keychain"     block-bash.sh "security unlock-keychain login.keychain"     BLOCK
expect "sudo security find-identity"  block-bash.sh "sudo security find-identity -v"              BLOCK
expect "pkill security (mgmt ok)"     block-bash.sh "pkill security"                              ALLOW
expect "notarytool store-credentials" block-bash.sh "xcrun notarytool store-credentials murmur"   BLOCK
expect "notarytool submit (ok)"       block-bash.sh "xcrun notarytool submit a.dmg --wait"        ALLOW
expect "cargo clippy --all-targets"   block-bash.sh "cargo clippy --all-targets -- -D warnings"   BLOCK
expect "cargo test --lib (ok)"        block-bash.sh "cargo test --lib"                            ALLOW
expect "bash scripts/ci.sh (ok)"      block-bash.sh "bash scripts/ci.sh"                          ALLOW
expect "codesign --deep"              block-bash.sh "codesign --deep --sign H a.app"              BLOCK
expect "codesign helper (ok)"         block-bash.sh "codesign --options runtime --sign H helper"  ALLOW
expect "rm -rf / "                    block-bash.sh "rm -rf /"                                    BLOCK
expect "rm -rf build (ok)"            block-bash.sh "rm -rf src-tauri/target/tmp"                 ALLOW

echo "── secret-scan.sh ──"
expect "non-commit is ignored"        secret-scan.sh "git status"                                 ALLOW

# Deterministic scan test in a throwaway repo (independent of the live staging area).
ss_case() { # <label> <staged-line> <BLOCK|ALLOW>
  local label="$1" line="$2" want="$3" td rc got
  td="$(mktemp -d)"
  (
    cd "$td" && git init -q && git config user.email t@t && git config user.name t
    printf '%s\n' "$line" > f.txt && git add f.txt
    as_json "git commit -m x" | bash "$HOOKS/secret-scan.sh" >/dev/null 2>&1
  ); rc=$?
  [ "$rc" -eq 2 ] && got=BLOCK || got=ALLOW
  rm -rf "$td"
  if [ "$got" = "$want" ]; then printf '  ✅ %-34s %s\n' "$label" "$got"
  else printf '  ❌ %-34s got %s want %s\n' "$label" "$got" "$want"; fail=1; fi
}
ss_case "staged Anthropic key blocks"  'k="sk-ant-api03-abcdefghijklmnopqrstuvwxyz012"' BLOCK
ss_case "staged PEM key blocks"        '-----BEGIN OPENSSH PRIVATE KEY-----'            BLOCK
ss_case "staged real 64-hex blocks"    'dek="a3f1c92e77b04d55a3f1c92e77b04d55a3f1c92e77b04d55a3f1c92e77b04d55"' BLOCK
ss_case "dev DEK placeholder allowed"  'MURMUR_DEV_DEK=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' ALLOW
ss_case "plain code allowed"           'fn main() { println!("hi"); }'                  ALLOW

if [ "$fail" -eq 0 ]; then echo "guardrail self-test: PASS"; else echo "guardrail self-test: FAIL"; fi
exit "$fail"
