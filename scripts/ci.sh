#!/usr/bin/env bash
# Local CI gate for MeetNotes — the single command that must stay green.
# Runs: Rust lint + tests + build, the Angular build, and the headless core E2E.
# Usage: bash scripts/ci.sh
set -euo pipefail
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

echo "── cargo clippy (deny warnings) ──"
( cd src-tauri && cargo clippy --all-targets -- -D warnings )

echo "── cargo test ──"
( cd src-tauri && cargo test --quiet )

echo "── cargo build ──"
( cd src-tauri && cargo build )

echo "── ng lint ──"
npx ng lint

echo "── ng build ──"
npx ng build

echo "── headless core E2E (say → ffmpeg → Whisper → provider → Obsidian) ──"
bash scripts/e2e-core.sh

echo
echo "✅ CI: all gates green"
