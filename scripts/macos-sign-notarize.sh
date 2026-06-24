#!/usr/bin/env bash
# TEMPLATE — code-sign + notarize the MeetNotes app bundle for distribution.
#
# This CANNOT run in this build environment: it needs a paid Apple Developer
# account, a "Developer ID Application" certificate in your login keychain, and a
# notarytool credential profile. Fill the variables and run it on your Mac.
#
# NOTE: Tauri can also sign during `npx tauri build` if you set the env vars
#   APPLE_SIGNING_IDENTITY, APPLE_ID, APPLE_PASSWORD (app-specific), APPLE_TEAM_ID
# and configure bundle signing in tauri.conf.json. This script is the manual
# fallback / reference for the codesign → notarize → staple flow.
set -euo pipefail

: "${DEVELOPER_ID:?set DEVELOPER_ID='Developer ID Application: Your Name (TEAMID)'}"
: "${APP_PATH:?set APP_PATH=path to MeetNotes.app produced by 'npx tauri build'}"
: "${NOTARY_PROFILE:?set NOTARY_PROFILE=name of a stored notarytool profile}"
# One-time profile setup:
#   xcrun notarytool store-credentials "$NOTARY_PROFILE" \
#     --apple-id you@example.com --team-id TEAMID --password <app-specific-password>

echo "1) Codesign (hardened runtime + secure timestamp)…"
codesign --force --deep --options runtime --timestamp \
  --sign "$DEVELOPER_ID" "$APP_PATH"
codesign --verify --strict --verbose=2 "$APP_PATH"

echo "2) Zip + submit for notarization (waits for the result)…"
ZIP="${APP_PATH%.app}.zip"
ditto -c -k --keepParent "$APP_PATH" "$ZIP"
xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait

echo "3) Staple the notarization ticket…"
xcrun stapler staple "$APP_PATH"
xcrun stapler validate "$APP_PATH"

echo "✅ signed + notarized: $APP_PATH"
