#!/usr/bin/env bash
# Capture the Murmur README screenshots (real Angular UI + mocked Tauri IPC).
#
#   1. start the Angular dev server:   npm start        (serves :1420)
#   2. run this:                       bash scripts/screenshots/run.sh [shot...]
#
# Playwright is NOT a package.json dependency (it's a dev-only capture tool), so
# we resolve it from whichever npx cache already has it — no manifest change, no
# install. Chromium comes from the shared ~/Library/Caches/ms-playwright cache.
set -euo pipefail
cd "$(dirname "$0")/../.."

# Find an npx-cached playwright (prefer a stable 1.61.x).
NP=""
for d in "$HOME"/.npm/_npx/*/node_modules; do
  if [ -e "$d/playwright/package.json" ]; then
    v=$(node -e "console.log(require('$d/playwright/package.json').version)" 2>/dev/null || true)
    case "$v" in 1.61.*) NP="$d"; break;; esac
    [ -z "$NP" ] && NP="$d"
  fi
done
if [ -z "$NP" ]; then
  echo "No cached playwright found. Run once:  npx playwright@1.61.1 --version" >&2
  exit 1
fi

echo "[screenshots] using playwright at: $NP"
PLAYWRIGHT_PATH="$NP/playwright" node scripts/screenshots/capture.mjs "$@"
