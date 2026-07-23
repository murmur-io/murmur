# MeetNotes — Release checklist (the only steps left to be prod-ready)

> **⚠️ SUPERSEDED (2026-06-27).** This doc predates the Murmur rename — it says
> `MeetNotes.app` and ad-hoc signing. The current, proven release runbook is the
> **`/release-murmur`** skill (`.claude/skills/release-murmur/SKILL.md`): universal build →
> Developer-ID sign **by identity hash** → notarize → staple → `gh release`. Use that.
> The notes below are kept only for the first-run / mic / system-audio manual checks.

Everything that can be built and verified without a physical Mac + Apple account is **done
and green**: `bash scripts/ci.sh` (clippy `-D warnings`, 34 tests, ng lint/build, core +
mixing headless E2E) and `bash scripts/release.sh` (builds a `MeetNotes.app` that
ad-hoc-signs and passes `codesign --verify --deep --strict`, plus a functional `.dmg`).

The four items below **cannot be verified headless** — they need your desktop session,
microphone, Screen Recording permission, live audio, and a paid Apple Developer ID. Each
is ~5 minutes on your Mac.

## 1. First run + microphone  → closes: real mic capture + GUI
```bash
# one-time: install or verify the immutable, checksum-pinned default Whisper model
scripts/ensure-whisper-model.sh
npx tauri dev
```
In the window → **Settings**: set the Vault folder (model is auto-found). Then
**Record → speak → Stop**. ✅ Confirm a note appears in the vault and the transcript is right.

## 2. System audio  → closes: live ScreenCaptureKit capture
**Settings → enable "Capture system audio."** Start recording, play any audio/call, Stop.
macOS prompts for **Screen Recording** the first time → Allow (System Settings → Privacy).
✅ Confirm the note includes the other side of the conversation.

## 3. Signed, notarized, distributable build  → closes: Developer-ID signing + notarization + styled DMG
Requires a paid **Apple Developer ID**.
```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: <Your Name> (<TEAMID>)"
export APPLE_ID="you@example.com" APPLE_PASSWORD="<app-specific-password>" APPLE_TEAM_ID="<TEAMID>"
npx tauri build            # signed .app + styled .dmg (Finder layout step runs on a desktop)
bash scripts/macos-sign-notarize.sh   # if tauri build didn't notarize: notarytool + stapler
spctl -a -vvv "src-tauri/target/release/bundle/macos/MeetNotes.app"   # expect: accepted, Notarized Developer ID
```

**When 1–3 pass, MeetNotes is prod-ready.** Until then the codebase is feature-complete and
verified to the limit of a headless environment.
