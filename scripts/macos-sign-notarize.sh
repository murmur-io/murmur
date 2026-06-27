#!/usr/bin/env bash
# Build → Developer-ID-sign (hardened runtime) → notarize → staple a UNIVERSAL Murmur DMG
# for clean download-and-open distribution (no xattr / no "Open Anyway").
#
# Run this on your Mac AFTER your Apple Developer Program membership is ACTIVE and a
# "Developer ID Application" certificate is in your login keychain. It reads all secrets
# from the environment — nothing is hardcoded.
#
# ── Required environment ─────────────────────────────────────────────────────
#   DEVELOPER_ID   e.g. "Developer ID Application: Jakub Gawronski (TEAMID)"
#                  (find it: `security find-identity -v -p codesigning`)
# Notarization auth — EITHER a stored profile:
#   NOTARY_PROFILE name of a notarytool keychain profile, created once with:
#     xcrun notarytool store-credentials "$NOTARY_PROFILE" \
#       --apple-id you@example.com --team-id TEAMID --password <app-specific-password>
# OR pass the three directly:
#   APPLE_ID        your Apple ID email (the one with the membership)
#   APPLE_PASSWORD  an app-specific password (appleid.apple.com → App-Specific Passwords)
#   APPLE_TEAM_ID   your 10-char Team ID
#
# Usage:  DEVELOPER_ID="Developer ID Application: … (TEAMID)" NOTARY_PROFILE=murmur \
#           scripts/macos-sign-notarize.sh
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
ENTITLEMENTS="$ROOT/src-tauri/entitlements.plist"
VERSION="$(grep -m1 '"version"' "$ROOT/src-tauri/tauri.conf.json" | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
APP="$ROOT/src-tauri/target/universal-apple-darwin/release/bundle/macos/Murmur.app"
OUT_DMG="$HOME/Desktop/Murmur-$VERSION.dmg"

: "${DEVELOPER_ID:?set DEVELOPER_ID='Developer ID Application: Your Name (TEAMID)' — see security find-identity -v -p codesigning}"
[ -f "$ENTITLEMENTS" ] || { echo "missing $ENTITLEMENTS" >&2; exit 1; }

# Notarization auth: prefer a stored profile, else require the apple-id trio.
NOTARY_ARGS=()
if [ -n "${NOTARY_PROFILE:-}" ]; then
  NOTARY_ARGS=(--keychain-profile "$NOTARY_PROFILE")
else
  : "${APPLE_ID:?set APPLE_ID or NOTARY_PROFILE}"
  : "${APPLE_PASSWORD:?set APPLE_PASSWORD (app-specific) or NOTARY_PROFILE}"
  : "${APPLE_TEAM_ID:?set APPLE_TEAM_ID or NOTARY_PROFILE}"
  NOTARY_ARGS=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
fi

echo "▸ Murmur $VERSION — Developer ID notarized universal build"
echo "  identity: $DEVELOPER_ID"

echo "1) Building universal (.app, arm64 + x86_64)…"
npx tauri build --target universal-apple-darwin --bundles app
[ -d "$APP" ] || { echo "universal .app not found at $APP" >&2; exit 1; }

echo "2) Codesigning INSIDE-OUT (nested helpers first, app last — NO --deep)…"
# --deep mis-signs nested Mach-Os and breaks notarization once a helper is bundled
# (deprecated since macOS 13; reproduces Tauri #11992). Sign each embedded sidecar FIRST
# with hardened runtime + timestamp, THEN seal the app bundle last.
for HELPER in \
  "$APP/Contents/Resources/meetnotes-sysaudio" \
  "$APP/Contents/Resources/meetnotes-audiocap" \
  "$APP/Contents/Resources/meetnotes-aeccap"; do
  if [ -f "$HELPER" ]; then
    echo "   • helper: $(basename "$HELPER")"
    codesign --force --options runtime --timestamp \
      --entitlements "$ENTITLEMENTS" --sign "$DEVELOPER_ID" "$HELPER"
  fi
done
codesign --force --options runtime --timestamp \
  --entitlements "$ENTITLEMENTS" --sign "$DEVELOPER_ID" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

echo "3) Building the DMG (with Applications alias)…"
STAGE="$(mktemp -d)/Murmur"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/Murmur.app"
ln -s /Applications "$STAGE/Applications"
rm -f "$OUT_DMG"
hdiutil create -volname "Murmur" -srcfolder "$STAGE" -ov -format UDZO "$OUT_DMG"
codesign --force --timestamp --sign "$DEVELOPER_ID" "$OUT_DMG"

echo "4) Notarizing the DMG (waits for Apple)…"
xcrun notarytool submit "$OUT_DMG" "${NOTARY_ARGS[@]}" --wait

echo "5) Stapling the notarization ticket…"
xcrun stapler staple "$OUT_DMG"
xcrun stapler validate "$OUT_DMG"
spctl -a -vvv -t open --context context:primary-signature "$OUT_DMG" 2>&1 | tail -2 || true

echo "✅ Notarized universal DMG: $OUT_DMG"
echo "   Upload to the release:"
echo "     gh release upload v$VERSION --repo JakubGawr/murmur \"$OUT_DMG\" --clobber"
