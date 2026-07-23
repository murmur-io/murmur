#!/usr/bin/env bash
# Build → Developer-ID-sign (hardened runtime) → notarize → staple a UNIVERSAL Murmur DMG
# for clean download-and-open distribution (no xattr / no "Open Anyway").
#
# Run this on your Mac AFTER your Apple Developer Program membership is ACTIVE and a
# "Developer ID Application" certificate is in your login keychain. It reads all secrets
# from the environment — nothing is hardcoded.
#
# ── Required environment ─────────────────────────────────────────────────────
#   DEVELOPER_ID   40-hex identity hash supplied by the user from an interactive terminal.
#                  The agent shell must never inspect the Keychain with `security`.
# Notarization auth — EITHER a stored profile:
#   NOTARY_PROFILE name of a notarytool keychain profile, created once with:
#     xcrun notarytool store-credentials "$NOTARY_PROFILE" \
#       --apple-id you@example.com --team-id TEAMID --password <app-specific-password>
# OR pass the three directly:
#   APPLE_ID        your Apple ID email (the one with the membership)
#   APPLE_PASSWORD  an app-specific password (appleid.apple.com → App-Specific Passwords)
#   APPLE_TEAM_ID   your 10-char Team ID
#
# Usage:  DEVELOPER_ID="<user-supplied-40-hex-hash>" NOTARY_PROFILE=murmur \
#           scripts/macos-sign-notarize.sh
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
ENTITLEMENTS="$ROOT/src-tauri/entitlements.plist"
ENTITLEMENTS_APP="$ROOT/src-tauri/entitlements-app.plist"
PROFILE="$ROOT/src-tauri/Murmur_Developer_ID.provisionprofile"
VERSION="$(grep -m1 '"version"' "$ROOT/src-tauri/tauri.conf.json" | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
# NOTE: the cargo target dir moved to the WORKSPACE ROOT (`$ROOT/target`) when the brain-sidecar
# extraction introduced a virtual workspace (members: src-tauri + crates/murmur-brain). It was
# `$ROOT/src-tauri/target/...` before. Entitlements/profile below stay under src-tauri/ (source files).
APP="$ROOT/target/universal-apple-darwin/release/bundle/macos/Murmur.app"
OUT_DMG="$HOME/Desktop/Murmur-$VERSION.dmg"

: "${DEVELOPER_ID:?set DEVELOPER_ID to the user-supplied 40-hex Developer ID identity hash}"
if ! printf '%s\n' "$DEVELOPER_ID" | grep -Eq '^[0-9A-Fa-f]{40}$'; then
  echo "DEVELOPER_ID must be the exact user-supplied 40-hex identity hash" >&2
  exit 1
fi
[ -f "$ENTITLEMENTS" ] || { echo "missing $ENTITLEMENTS" >&2; exit 1; }
[ -f "$ENTITLEMENTS_APP" ] || { echo "missing $ENTITLEMENTS_APP" >&2; exit 1; }
[ -f "$PROFILE" ] || { echo "missing provisioning profile $PROFILE" >&2; exit 1; }

# PREFLIGHT: the DP-keychain ACL items (biometric master KEK + account MK) need the RESTRICTED
# keychain-access-groups entitlement, authorized ONLY by this embedded provisioning profile. A
# missing/EXPIRED/mismatched profile AMFI-kills launch even though codesign + notarization pass
# (the 0.7.1 incident). Fail the build NOW rather than ship a bundle that dies on the user's Mac.
PROFILE_PLIST="$(mktemp -t murmur-profile.XXXXXX)"
trap 'rm -f "$PROFILE_PLIST"' EXIT
if ! /usr/bin/openssl smime -verify -inform DER -noverify -in "$PROFILE" -out "$PROFILE_PLIST" >/dev/null 2>&1; then
  echo "could not decode provisioning profile without Keychain access" >&2
  exit 1
fi
PROFILE_EXP="$(plutil -extract ExpirationDate raw "$PROFILE_PLIST" 2>/dev/null || true)"
if [ -n "$PROFILE_EXP" ]; then
  # ExpirationDate is ISO-8601 (e.g. 2027-01-01T00:00:00Z). Compare epoch seconds.
  EXP_EPOCH="$(date -j -f "%Y-%m-%dT%H:%M:%SZ" "$PROFILE_EXP" +%s 2>/dev/null || echo 0)"
  NOW_EPOCH="$(date +%s)"
  if [ "$EXP_EPOCH" != "0" ] && [ "$EXP_EPOCH" -le "$NOW_EPOCH" ]; then
    echo "provisioning profile EXPIRED ($PROFILE_EXP) — regenerate the Developer-ID profile before releasing" >&2
    exit 1
  fi
  echo "   provisioning profile valid until $PROFILE_EXP"
fi

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

require_universal_macho() {
  local binary="$1" arches
  [ -f "$binary" ] || { echo "required universal binary missing: $binary" >&2; exit 1; }
  arches="$(lipo -archs "$binary")" || { echo "lipo could not inspect: $binary" >&2; exit 1; }
  case " $arches " in *" arm64 "*) ;; *) echo "arm64 slice missing: $binary ($arches)" >&2; exit 1 ;; esac
  case " $arches " in *" x86_64 "*) ;; *) echo "x86_64 slice missing: $binary ($arches)" >&2; exit 1 ;; esac
}
require_universal_macho "$APP/Contents/MacOS/Murmur"
for REQUIRED_HELPER in \
  meetnotes-sysaudio meetnotes-audiocap meetnotes-aeccap meetnotes-calendar murmur-brain; do
  require_universal_macho "$APP/Contents/Resources/$REQUIRED_HELPER"
done
echo "   verified universal slices for app + every bundled helper"

# Embed the Developer-ID provisioning profile that AUTHORIZES keychain-access-groups (the
# data-protection keychain holding the biometric master KEK + account MK). Without it, that restricted
# entitlement has no authorization and the kernel AMFI-kills launch even though codesign + notarization
# both pass. Only the main .app carries it — the nested helpers have no keychain entitlement.
cp "$PROFILE" "$APP/Contents/embedded.provisionprofile"
echo "   embedded provisioning profile ($(basename "$PROFILE"))"

echo "2) Codesigning INSIDE-OUT (nested helpers first, app last — NO --deep)…"
# --deep mis-signs nested Mach-Os and breaks notarization once a helper is bundled
# (deprecated since macOS 13; reproduces Tauri #11992). Sign each embedded sidecar FIRST
# with hardened runtime + timestamp, THEN seal the app bundle last.
# Glob every legacy `meetnotes-*` capture helper and include the product-named `murmur-brain`
# explicitly. A hardcoded list that omitted meetnotes-calendar is exactly what made the first 0.5.0
# notarization Invalid ("meetnotes-calendar: binary is not signed").
for HELPER in "$APP/Contents/Resources/"meetnotes-* "$APP/Contents/Resources/murmur-brain"; do
  if [ -f "$HELPER" ]; then
    echo "   • helper: $(basename "$HELPER")"
    codesign --force --options runtime --timestamp \
      --entitlements "$ENTITLEMENTS" --sign "$DEVELOPER_ID" "$HELPER"
  fi
done
# The main app gets the app-only entitlements (adds keychain-access-groups + application-identifier,
# authorized by the embedded profile above); the nested helpers keep the plain entitlements.plist so
# they carry NO restricted keychain entitlement (they have no profile → would be AMFI-killed).
codesign --force --options runtime --timestamp \
  --entitlements "$ENTITLEMENTS_APP" --sign "$DEVELOPER_ID" "$APP"
for HELPER in "$APP/Contents/Resources/"meetnotes-* "$APP/Contents/Resources/murmur-brain"; do
  if [ -f "$HELPER" ]; then
    codesign --verify --strict --verbose=2 "$HELPER"
  fi
done
codesign --verify --strict --verbose=2 "$APP"

# POST-SIGN ASSERTION: prove the shipped bundle actually carries the keychain-access-groups
# entitlement AND the embedded profile — the two things whose absence silently AMFI-kills launch
# despite a passing codesign/notarization. Fail loudly here instead of on the user's Mac.
[ -f "$APP/Contents/embedded.provisionprofile" ] || {
  echo "signed app is MISSING Contents/embedded.provisionprofile — launch would be AMFI-killed" >&2; exit 1; }
if ! codesign -d --entitlements :- "$APP" 2>/dev/null | grep -q "keychain-access-groups"; then
  echo "signed app is MISSING the keychain-access-groups entitlement — biometric lock/sharing would fail (and launch may be AMFI-killed)" >&2
  exit 1
fi
echo "   verified: embedded profile present + keychain-access-groups entitlement signed in"

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
echo "     gh release upload v$VERSION --repo murmur-io/murmur \"$OUT_DMG\" --clobber"
