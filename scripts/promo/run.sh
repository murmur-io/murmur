#!/usr/bin/env bash
# Build Murmur's promo video end to end: record → compose → encode.
#
#   1. serve the frontend (no Rust core needed — the mock replaces Tauri):
#        npx ng serve --host 127.0.0.1 --port 4310 --watch=false
#   2. build the film:
#        MURMUR_URL=http://127.0.0.1:4310 bash scripts/promo/run.sh
#
# Stages can be run individually — `... run.sh record`, `render`, `encode`, or
# `hero` for the silent landing-page loop. With no argument it runs everything.
#
# Playwright is resolved from whichever npx cache already has it, exactly as
# scripts/screenshots/run.sh does: it is a dev-only capture tool and is
# deliberately NOT a package.json dependency (no new FE deps without approval).
set -euo pipefail
cd "$(dirname "$0")/../.."

STAGE="${1:-all}"
FPS="${PROMO_FPS:-60}"
export PROMO_DIR="${PROMO_DIR:-.promo}"

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
export PLAYWRIGHT_PATH="$NP/playwright"

run_record() {
  echo "── 1/3  record ─────────────────────────────────────────"
  node scripts/promo/record.mjs
}

run_render() {
  echo "── 2/3  compose ────────────────────────────────────────"
  node scripts/promo/render.mjs --fps "$FPS"
}

run_encode() {
  echo "── 3/3  encode ─────────────────────────────────────────"
  bash scripts/promo/encode.sh "$FPS"
}

# The landing-page hero loop: ambient, silent-safe, no captions and no end card,
# cut from the scene with motion of its own. Kept SHORT because a hero loop is
# page weight on the critical path — the research brief's 4–20 s band.
run_hero() {
  echo "── hero loop ───────────────────────────────────────────"
  node scripts/promo/render.mjs --fps "$FPS" --no-captions --no-endcard \
    --scenes record --to 9000 --out "$PROMO_DIR/render-hero"
  bash scripts/promo/encode.sh "$FPS" "$PROMO_DIR/render-hero" "$PROMO_DIR/out-hero"
}

case "$STAGE" in
  record) run_record ;;
  render) run_render ;;
  encode) run_encode ;;
  hero)   run_hero ;;
  all)    run_record; run_render; run_encode ;;
  *) echo "usage: run.sh [record|render|encode|hero|all]" >&2; exit 1 ;;
esac
