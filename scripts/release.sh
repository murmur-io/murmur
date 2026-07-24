#!/usr/bin/env bash
# DEPRECATED — smoke test only, NOT the release path.
# This targets the stale bundle name (MeetNotes.app); the real universal release
# is Murmur.app at the workspace-root target/, driven by the `release-murmur`
# skill + scripts/macos-sign-notarize.sh. Use this file only to prove the bundle
# builds, ad-hoc-signs, and packages into a functional .dmg on a headless box.
set -euo pipefail
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true
cd "$(dirname "$0")/.."

APP="src-tauri/target/release/bundle/macos/MeetNotes.app"
OUT="src-tauri/target/release/bundle/dmg"   # under target/ → gitignored
mkdir -p "$OUT"

echo "── 1. Build release .app (unsigned) ──"
npx tauri build --bundles app

echo "── 2. Ad-hoc codesign + verify (Developer-ID identity is a separate, account-gated step) ──"
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
echo "   ✅ bundle valid + signable (Signature=adhoc; swap in a Developer ID to distribute)"

echo "── 3. Functional .dmg via hdiutil (no Finder; Tauri's styled DMG needs a desktop) ──"
hdiutil create -volname "MeetNotes" -srcfolder "$APP" -ov -format UDZO "$OUT/MeetNotes.dmg"
hdiutil verify "$OUT/MeetNotes.dmg"
echo "   ✅ $OUT/MeetNotes.dmg"

cat <<'NEXT'

For a Gatekeeper-distributable release (needs a real Mac + paid Apple Developer ID):
  • Developer-ID sign:  codesign --force --deep --options runtime --timestamp \
                          --sign "Developer ID Application: <you> (<TEAMID>)" "$APP"
  • Notarize + staple:  scripts/macos-sign-notarize.sh   (xcrun notarytool + stapler)
  • Styled .dmg:        npx tauri build   (full; its bundle_dmg.sh layout step needs Finder)
NEXT
echo "✅ release packaging complete (headless-verifiable steps)"
