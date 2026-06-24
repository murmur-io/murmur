#!/usr/bin/env bash
# Build the distributable MeetNotes.app (release, unsigned). Verified to work headless.
# The .dmg layout, code-signing, and notarization are GUI / Apple-account steps — run
# them on a real Mac (see the printout at the end).
set -euo pipefail
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true
cd "$(dirname "$0")/.."

echo "── Building release .app (unsigned) ──"
npx tauri build --bundles app
APP="src-tauri/target/release/bundle/macos/MeetNotes.app"
echo "✅ Built: $APP"

cat <<'NEXT'

Release steps that need a real Mac (not possible headless / in CI):
  1. .dmg — `npx tauri build` (no --bundles flag) also builds a .dmg, but its layout
     step (bundle_dmg.sh) uses Finder/AppleScript and needs a desktop session.
  2. Sign + notarize — set APPLE_SIGNING_IDENTITY + APPLE_ID / APPLE_PASSWORD /
     APPLE_TEAM_ID and re-run `tauri build`, or run scripts/macos-sign-notarize.sh
     against the .app. Requires a paid Apple Developer ID.
NEXT
