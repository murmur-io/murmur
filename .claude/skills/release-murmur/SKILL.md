---
name: release-murmur
description: Cut a signed, notarized macOS release of Murmur (Tauri 2 + Angular 22). The exact, proven step-by-step runbook — preflight gates → version bump (+ Cargo.lock sync) → QueaT commit → PR-merge to the `murmur` trunk (never direct-push) → rustup targets → stop dev → universal build → Developer-ID sign BY IDENTITY HASH (the Polish-ń cert gotcha) → DMG → notarize → staple/spctl → gh release create + upload. Use whenever the user wants to ship, release, cut a version, build a distributable .app/.dmg, sign/notarize, or publish a GitHub release of Murmur. Supersedes the stale docs/RELEASE-CHECKLIST.md.
---

# /release-murmur — the Murmur macOS release runbook

You are cutting a distributable build of **Murmur** — a local-first macOS app
(Tauri 2.11, Rust crate `murmur` / bin `Murmur` / lib `meetnotes_lib` + Angular 22
zoneless). Output is a **universal (arm64 + x86_64), Developer-ID-signed,
notarized, stapled `.dmg`** attached to a GitHub release on `murmur-io/murmur`.

This runbook is the proven v0.3.0 process. Follow it **in order** — do not skip a
stage until the prior one is green. It **supersedes `docs/RELEASE-CHECKLIST.md`**
(stale: that doc and `scripts/release.sh` still say `MeetNotes.app`; the real bundle
is `Murmur.app` and the real sign/notarize path is `scripts/macos-sign-notarize.sh`).

A fresh agent can cut a release by following this verbatim. Two halves:
**headless-safe** (gates, bump, commit, PR-merge) can run anywhere; the
**Mac-only** half (build, sign, notarize) needs the user's signed-in macOS session
with the Developer-ID cert + Apple notary creds. If a Mac/credential is missing,
stop at that boundary and hand back the exact command the user must run.

## Hard constraints (verify every one — they have each bitten before)

1. **`gh` active account MUST be `JakubGawr`.** `gh auth status` → "Active account: true"
   on `JakubGawr` (a second account `jakub-united` is also logged in — do not use it).
2. **Commit author MUST be `QueaT <kgm004a@gmail.com>`.** NO `Co-Authored-By: Claude`,
   NO Claude trailers anywhere in release commits/PR bodies. (Repo git config is already
   `QueaT` / `kgm004a@gmail.com`; the v0.3.0 bump commit `2038177` proves the shape.)
3. **NEVER `git push origin murmur` / `…main` directly.** The trunk branch is `murmur`;
   integrate via a PR (`gh pr create … --base murmur` → `gh pr merge`). An environment-level
   `block-bash.sh` guard rejects direct pushes to main/master — a PR is the only path.
4. **App identifier `com.meetnotes.app` is IMMUTABLE.** Never change `identifier` in
   `src-tauri/tauri.conf.json`. Changing it breaks macOS TCC (mic / screen-recording)
   permission continuity and the keychain ACL for the SQLCipher DEK / master KEK.
5. **Sign by the IDENTITY HASH, not the name.** See stage 8 — the cert CN contains a
   Polish `ń` and the name string fails to match.
6. **Three version files must agree** (`package.json`, `src-tauri/tauri.conf.json`,
   `src-tauri/Cargo.toml`) **and `Cargo.lock` must be re-pinned** (stage 2).

---

## Stage 0 — Preflight gates (must be GREEN before anything)

Work on a **feature branch** (never the trunk). Pick the version (semver bump from the
latest tag — existing releases: `v0.1.0`, `v0.2.0`, `v0.3.0`).

```bash
source "$HOME/.cargo/env"
cd /Users/jakubgawronski/Projects/meetnotes
git rev-parse --abbrev-ref HEAD            # confirm a feature branch, NOT murmur/main
gh auth status                            # confirm Active account: JakubGawr
git log -1 --format='%an <%ae>'           # confirm QueaT <kgm004a@gmail.com>
bash scripts/ci.sh                        # the full gate — must end "✅ CI: all gates green"
```

`scripts/ci.sh` runs: swiftc typecheck of the ScreenCaptureKit sidecar → `cargo clippy
--all-targets -- -D warnings` → `cargo test` → `cargo build` → `npx ng lint` →
`npx ng build` → headless core E2E (`e2e-core.sh`) → headless mixing E2E (`e2e-mix.sh`).

> **Note — clippy is fine HERE.** `scripts/ci.sh` runs `clippy --all-targets` deliberately
> as the release gate. That is different from the *inner dev loop*, where you use
> `cargo test --lib` only and avoid `clippy --all-targets` (it thrashes the
> openssl/sqlcipher build profile and times out). For a release you DO want the full gate.

Do not proceed unless CI is green.

> **Also refresh the public copy before you ship.** Run **`/sync-release-copy`** now —
> it checks the landing page (`landing/index.html`) and the GitHub repo description
> against what the app actually does as of this release, and lands any needed edits
> (landing via a PR that auto-deploys, description via `gh repo edit`). Skipping it
> ships a release whose marketing describes an older app.

## Stage 1 — Pick the version

```bash
NEW=0.4.0   # example — set to the real bump
```

## Stage 2 — Bump version in all 3 files + sync the lock

Edit the `version` field in each (keep `identifier` untouched in tauri.conf.json):
- `package.json` → `"version": "<NEW>"`
- `src-tauri/tauri.conf.json` → `"version": "<NEW>"`
- `src-tauri/Cargo.toml` → `version = "<NEW>"`

Then **re-pin `Cargo.lock`** (the `murmur` package entry must match, or the build/CI
drifts):

```bash
( cd src-tauri && cargo update -p murmur --precise "$NEW" )
grep -A1 '^name = "murmur"' src-tauri/Cargo.lock   # confirm version = "<NEW>"
```

## Stage 3 — Commit as QueaT (no Claude trailers)

```bash
git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "chore(release): bump version to $NEW"
git log -1 --format='%an <%ae>%n%b'   # author QueaT, body has NO Co-Authored-By / Claude
```

> **CALLOUT — no Claude trailers.** If your environment auto-appends a
> `Co-Authored-By: Claude` trailer, strip it (`git commit --amend`) before pushing.
> Release history is QueaT-only.

## Stage 4 — Merge to the `murmur` trunk via PR (NEVER direct push)

```bash
BR=$(git rev-parse --abbrev-ref HEAD)
git push -u origin "$BR"                                  # push the FEATURE branch (allowed)
gh pr create -R murmur-io/murmur --base murmur --head "$BR" \
  --title "chore(release): $NEW" --body "Version bump + release $NEW."
gh pr merge <pr#|url> -R murmur-io/murmur --merge          # merge commit onto murmur
git checkout murmur && git pull origin murmur             # local trunk now has the bump
```

> **CALLOUT.** `--base murmur` (the trunk), not `main`/`master`. Direct `git push origin
> murmur` is blocked by the environment guard — the PR is mandatory. `gh` account = JakubGawr.

---

> **Mac-only boundary.** Stages 5–11 need the user's macOS desktop session with the
> "Developer ID Application" cert in the login keychain and (for stage 10) Apple notary
> credentials. If running headless, STOP here and hand the user stages 5–11 verbatim.

## Stage 5 — Add the universal Rust targets (once per machine)

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

## Stage 6 — STOP the dev server

The running `tauri dev` (from `/tauri-dev`) holds the `target` cargo lock (at the WORKSPACE
ROOT since the brain-sidecar extraction made this a virtual workspace) — a release build blocks on it.

```bash
pkill -f 'tauri dev' ; pkill -x Murmur ; pkill -f 'target/debug/Murmur' || true
```

## Stage 7 — Universal build

The full product IS the default build. The on-device brain (mistralrs) now lives in the
`crates/murmur-brain` workspace member, compiled to the `meetnotes-brain` helper; the embedder/NER
(candle) are always compiled in the app crate and activate at runtime on model-presence. NO
`--features` flag. `MISTRALRS_METAL_PRECOMPILE=0` is baked into the WORKSPACE-ROOT `.cargo/config.toml
[env]` (moved there so the brain member inherits it; CLT-only Mac → defer Metal-shader compile to
first run); keep it on the command line too as a guard. The `beforeBuildCommand` builds
`meetnotes-brain` universal (both arches + `lipo`) into `src-tauri/binaries/` before the app compiles.

```bash
source "$HOME/.cargo/env"
MISTRALRS_METAL_PRECOMPILE=0 npx tauri build --target universal-apple-darwin --bundles app
APP="target/universal-apple-darwin/release/bundle/macos/Murmur.app"   # target is at the workspace ROOT now
lipo -archs "$APP/Contents/Resources/meetnotes-brain"   # NEW: MUST also print x86_64 arm64 (the brain helper)
lipo -archs "$APP/Contents/MacOS/Murmur"     # MUST print: x86_64 arm64
```

## Stage 8 — Developer-ID sign — BY IDENTITY HASH (the ń gotcha)

> **CALLOUT — DO NOT pass the identity by name.** The cert's Common Name renders as
> `Jakub Gawroński (BVF778E5QD)` — the real name contains a Polish `ń`, so passing the
> name string to `codesign --sign` fails with **"no identity found."** Resolve the
> 40-char SHA-1 **hash** and sign with that.

```bash
HASH=$(security find-identity -v -p codesigning \
        | grep 'Developer ID Application' | head -1 | awk '{print $2}')
echo "signing identity: $HASH"   # 40-hex; non-empty or STOP (cert missing)

codesign --force --deep --options runtime --timestamp \
  --entitlements src-tauri/entitlements.plist --sign "$HASH" "$APP"

codesign --verify --deep --strict --verbose=2 "$APP"
# expect: flags=0x10000(runtime) ; Authority=Developer ID Application: … (BVF778E5QD)
```

`src-tauri/entitlements.plist` (hardened-runtime) is REQUIRED here — it grants
`allow-jit` + `allow-unsigned-executable-memory` (WKWebView JS engine),
`disable-library-validation` (the ScreenCaptureKit Swift sidecar + whisper.cpp/Metal
dylibs aren't Team-signed), and `device.audio-input` (mic TCC). Team ID is `BVF778E5QD`.

## Stage 9 — Build the signed DMG

```bash
VER=$(grep -m1 '"version"' src-tauri/tauri.conf.json | sed -E 's/.*"([0-9][^"]*)".*/\1/')
DMG="$HOME/Desktop/Murmur-$VER.dmg"
STAGE="$(mktemp -d)/Murmur"; mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/Murmur.app"
ln -s /Applications "$STAGE/Applications"          # drag-to-install affordance
rm -f "$DMG"
hdiutil create -volname "Murmur" -ov -format UDZO -srcfolder "$STAGE" "$DMG"
codesign --force --timestamp --sign "$HASH" "$DMG"
```

## Stage 10 — Notarize (needs Apple credentials) + staple

Auth is EITHER a stored notarytool keychain profile OR the Apple-ID trio. Team ID `BVF778E5QD`.

```bash
# Option A — stored profile (created once via: xcrun notarytool store-credentials <PROFILE>
#   --apple-id you@example.com --team-id BVF778E5QD --password <app-specific-password>)
xcrun notarytool submit "$DMG" --keychain-profile <PROFILE> --wait

# Option B — pass creds directly
xcrun notarytool submit "$DMG" \
  --apple-id <you@example.com> --password <app-specific-password> --team-id BVF778E5QD --wait

xcrun stapler staple "$DMG"
spctl -a -vvv -t open --context context:primary-signature "$DMG"
# expect: source=Notarized Developer ID ; accepted
```

> **CALLOUT — `scripts/macos-sign-notarize.sh` automates stages 7→10** (build + sign +
> DMG + notarize + staple + spctl) and reads `VERSION` from `tauri.conf.json`. **But** it
> signs by `$DEVELOPER_ID` *name* — prefer the hash. Either edit it to use `$HASH` or run
> stages 7–10 by hand. Env it expects: `DEVELOPER_ID="Developer ID Application: … (BVF778E5QD)"`
> plus `NOTARY_PROFILE` (or `APPLE_ID` + `APPLE_PASSWORD` + `APPLE_TEAM_ID`).

> **CALLOUT — signed-but-UN-notarized is still shippable.** If Apple notary creds aren't
> available, a **Developer-ID-signed** (hardened-runtime) build is enough to distribute —
> users do **first launch: right-click → Open** once. It is ALSO enough to live-test Touch
> ID + lock-at-rest + screen-share auto-relock, because those need only a **stable code
> signature** (each rebuilt *dev* binary has a new ad-hoc signature → re-prompts; a
> Developer-ID signature is stable). Note the right-click→Open caveat in the release body.

## Stage 11 — Publish the GitHub release

```bash
gh release create "v$VER" -R murmur-io/murmur --target murmur --latest \
  --title "Murmur $VER — <one-line>" \
  --notes "<changelog>" \
  "$DMG"
# already-created release? upload/replace the asset:
gh release upload "v$VER" -R murmur-io/murmur "$DMG" --clobber
```

If the DMG is signed but not yet notarized, add to `--notes`:
`First launch: right-click the app → Open (Developer-ID signed, notarization pending).`

## Post-release verification

```bash
gh release view "v$VER" -R murmur-io/murmur          # asset attached, marked Latest
git tag --list | tail -3                              # v$VER present
spctl -a -vvv -t open --context context:primary-signature "$DMG"   # if notarized
```

---

## Orchestrating this as a Workflow (optional)

An orchestrator may drive these stages with the **Workflow / agent-team tooling**
(`TaskCreate`/`TaskUpdate`/`Monitor`) instead of one linear pass — useful because the
build→sign→notarize leg is long-running and Mac-bound. Two shapes:

- **Pipeline (sequential, gated):** `preflight-gates` (stage 0) → `bump+commit` (1–3) →
  `pr-merge` (4) → [Mac boundary] → `universal-build` (5–7) → `sign` (8–9) →
  `notarize+staple` (10) → `publish` (11). Each stage is a node that must report PASS
  before the next starts; the notarize node may be long-running (`--wait` blocks on Apple)
  and should be a `run_in_background` Monitor-until-done step.
- **Fan-out pre-release gates:** run independent read-only checks in parallel —
  `cargo test --lib`, `ng lint`, `ng build`, version-triple-agreement, `gh`-account +
  commit-author identity — collect verdicts, and only enter the bump stage when all PASS.

Keep the **identity interlocks as preconditions** on every writing node: `gh`=JakubGawr,
author=QueaT, base=`murmur`, identifier unchanged, sign-by-hash. The Mac-only stages stay a
hard boundary — a headless orchestrator hands them to the user rather than faking them.

## Rules

- **Order is binding.** No stage N+1 until stage N is green. CI green is the entry gate.
- **Never invent a green.** `lipo` must show both arches; `codesign --verify` must show
  `flags=0x10000(runtime)` + `Developer ID Application`; `spctl` must say accepted. If a
  check doesn't pass, STOP and report — do not "assume it worked."
- **Identity is non-negotiable:** gh=JakubGawr, commit-author=QueaT (no Claude trailers),
  base=`murmur` via PR, identifier=`com.meetnotes.app` unchanged, sign **by hash**.
- **Headless honesty.** If there is no Mac / no cert / no notary creds, say so and hand the
  user the exact remaining commands — never claim a notarized DMG you didn't produce.
- **Don't trust the stale docs.** `docs/RELEASE-CHECKLIST.md` + `scripts/release.sh` predate
  the Murmur rename (they say `MeetNotes.app`, ad-hoc sign) — this runbook supersedes them.
