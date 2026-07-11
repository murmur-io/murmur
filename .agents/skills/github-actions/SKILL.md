---
name: github-actions
description: Author and maintain GitHub Actions workflows for THIS repo (Murmur — Tauri 2 Rust + Angular 22, macOS-first, heavy always-compiled ML build). The best-practice patterns tuned to Murmur — macos-14 runner, Swatinem/rust-cache + npm cache + whisper-model cache, SHA-pinned actions resolved live, least-privilege permissions, concurrency cancel, secrets by name, and the ci.sh-wraps-not-duplicates rule. Includes the release-workflow blueprint (CD) that stays a design, since notarization belongs to release-engineer. Use whenever the user wants to write/change a GitHub Actions workflow, tune CI runtime/caching, pin actions, or design a cloud release pipeline for Murmur.
---

# /github-actions — workflow best-practices for Murmur

Murmur's cloud CI is one file: `.github/workflows/ci.yml`, a **thin wrapper around `scripts/ci.sh`** on a macOS runner. These are the patterns that make a workflow HERE correct, fast, and secure. The overriding rule: **the workflow wraps `ci.sh` — it never re-implements the gate** (that's how local and cloud drift). See `/ci-maintenance` for the gate itself, `/add-ci-gate` to change what it checks.

## Non-negotiables for THIS repo

1. **`runs-on: macos-14`.** macOS is mandatory — `swiftc`/ScreenCaptureKit, whisper.cpp Metal, `say`/ffmpeg E2E, the universal build only exist on macOS. `macos-14` is Apple Silicon (matches the dev Mac + release arch). Never "save minutes" on a Linux runner: it would silently stop testing the macOS surface.
2. **Least privilege.** `permissions: contents: read` at the top. Add a narrower scope to a single job only if it genuinely needs it; never a blanket `write-all`.
3. **`concurrency` cancel-in-progress** keyed on the ref, so a new push to a PR kills the stale run.
4. **Pin every action to a full commit SHA** (never a floating tag) with a `# vX.Y.Z` comment. Resolve the SHA LIVE (next section) — a hallucinated SHA fails the run.
5. **No new action without user approval** (same policy as new deps/crates).
6. **Secrets by name only.** Reference `${{ secrets.NAME }}`; never echo a secret, never `set -x` a step that holds one. GitHub masks known secrets in logs — don't defeat it by transforming them.
7. **`MISTRALRS_METAL_PRECOMPILE: "0"`** in the job/workflow `env` (mirrors ci.sh; defers Metal-shader compile, harmless where full Xcode exists).

## Pin an action to a real SHA (do this, don't guess)

```bash
# lightweight tag → the ref sha IS the commit:
gh api repos/actions/checkout/git/refs/tags/v4.2.2 --jq '.object.sha'
# annotated tag (.object.type == "tag") → deref to the COMMIT sha:
gh api repos/Swatinem/rust-cache/git/refs/tags/v2.7.5 --jq '.object.sha'      # -> tag object sha
gh api repos/Swatinem/rust-cache/git/tags/<that-sha> --jq '.object.sha'       # -> commit sha (pin THIS)
```

Pin the **commit** sha, comment the version. Currently pinned in `ci.yml`:
`actions/checkout@…11bd719 # v4.2.2`, `actions/setup-node@…39370e3 # v4.1.0`,
`Swatinem/rust-cache@…c193711 # v2.9.1`, `taiki-e/install-action@…678b06b # v2.44.60`,
`actions/cache@…55cc834 # v6.1.0`.

**Cache-service deprecation (bitten 2026-07-05):** GitHub killed the legacy cache
backend — `actions/cache` < v4.2 and `Swatinem/rust-cache` < v2.7.8 are AUTO-FAILED
at job setup ("deprecated version of actions/cache"). If a run dies before the first
step with that error, bump BOTH cache actions to current tags (resolve SHAs live).

## Make it fast (or the team routes around it)

- **`Swatinem/rust-cache`** with `workspaces: src-tauri`, AFTER the toolchain is resolved. This is the single biggest win — the always-compiled ML tree (mistralrs/candle) is hundreds of MB cold.
- **`actions/setup-node` with `cache: npm`** + `npm ci` (not `npm install`) for a reproducible, cached FE install.
- **Prebuilt cargo tools via `taiki-e/install-action`** (`tool: cargo-audit,cargo-deny`) so `ci.sh` finds them on PATH and skips its slow `cargo install`.
- **Cache the heavy E2E inputs.** Since the 2026-07-05 release-parity change every PR runs the COMPLETE ci.sh (incl. audio E2E); the cost is tamed with a **whisper-model `actions/cache`** + `brew install ffmpeg` — the 142 MB model downloads once, not per PR. `MURMUR_CI_SKIP_E2E=1` is local-only.
- **Resolve the pinned toolchain with `rustup show`** (reads `rust-toolchain.toml` → 1.96.0 + clippy/rustfmt). Don't add a second `dtolnay/rust-toolchain` with a different version.
- `timeout-minutes` on every job (a hung ML link shouldn't burn the full 6 h).

## The canonical shape (already in `ci.yml`)

```yaml
on:
  pull_request: { branches: [murmur] }
  workflow_dispatch: {}
  schedule: [ { cron: "0 6 * * 1" } ]
permissions: { contents: read }
concurrency: { group: ci-${{ github.workflow }}-${{ github.ref }}, cancel-in-progress: true }
env: { MISTRALRS_METAL_PRECOMPILE: "0", CARGO_TERM_COLOR: always }
jobs:
  gate:   # every PR + weekly + on-demand — the COMPLETE ci.sh (release parity)
    runs-on: macos-14
    steps: [ checkout, setup-node(node-version-file: .nvmrc, cache:npm), rustup show,
             rust-cache(src-tauri), install-action(cargo-audit,cargo-deny),
             brew install ffmpeg, cache(whisper model), npm ci,
             run: bash scripts/ci.sh ]
```

## Verify before you push

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml')); print('yaml ok')"
command -v actionlint >/dev/null && actionlint .github/workflows/ci.yml || echo "(install actionlint to lint)"
# After it's on a branch, trigger and watch:
gh workflow run CI -R murmur-io/murmur                      # manual gate run
gh run watch -R murmur-io/murmur
```

## CD / release workflow — a BLUEPRINT, not a live pipeline

A cloud release (sign → notarize → staple → publish) is **the release-engineer's job**, and it collides with a hard repo rule: **keychain/`security`/`notarytool store-credentials` ops must be user-interactive** (they hang on the auth dialog headlessly — `block-bash.sh` blocks them). So do NOT stand up an auto-notarizing release workflow without the user explicitly deciding to move Developer-ID signing into cloud CI. If they do, the blueprint is:

- Trigger: `push` tag `v*` or `workflow_dispatch`.
- `macos-14`, import the Developer-ID cert from `${{ secrets.APPLE_CERT_P12_BASE64 }}` into a temporary keychain, build universal (`rustup target add aarch64-apple-darwin x86_64-apple-darwin` → `npx tauri build --target universal-apple-darwin`), sign **inside-out by identity HASH, never `--deep`** (`scripts/macos-sign-notarize.sh` already does this), notarize with `xcrun notarytool submit --apple-id/--password/--team-id` from secrets (NOT `--keychain-profile`, which needs the interactive store), staple, `spctl` verify, `gh release upload`.
- Secrets: `APPLE_CERT_P12_BASE64`, `APPLE_CERT_PASSWORD`, `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`, `APPLE_TEAM_ID` (`BVF778E5QD`). `com.meetnotes.app` immutable.

Until then, cutting a release stays the local `/release-murmur` flow. Say that honestly rather than implying CI can ship a notarized DMG today.
