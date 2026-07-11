---
name: ci-maintenance
description: Run, understand, and debug Murmur's CI — the local `scripts/ci.sh` gate and the GitHub Actions macOS PR-gate that wraps it. The map of every gate step (hooks selftest → swiftc → clippy → cargo test → cargo audit → cargo deny → cargo build → ng lint → ng build → E2E), how to reproduce a red CI run locally with the exact command CI uses, and every repo-specific gotcha (heavy always-compiled ML build, MISTRALRS_METAL_PRECOMPILE, clippy-in-loop timeout, macOS-only steps, the MURMUR_CI_SKIP_E2E toggle). Use whenever the user wants to run/fix/understand CI, triage a red pipeline, or check why the gate failed.
---

# /ci-maintenance — run & debug Murmur's CI gate

Murmur's "CI" is two layers of the SAME gate:

- **`scripts/ci.sh`** — the **single source of truth** for what "green" means. Runs locally and is what every contributor / release must pass.
- **`.github/workflows/ci.yml`** — a thin macOS wrapper that just calls `bash scripts/ci.sh`. Since 2026-07-05 the per-PR `gate` job runs the COMPLETE ci.sh (release parity — incl. the audio E2E, with a cached whisper model + brew ffmpeg); the same job also runs weekly (schedule) and on-demand (`workflow_dispatch`). Node comes from `.nvmrc` (Angular 22 needs >= 24.15). `MURMUR_CI_SKIP_E2E=1` is a LOCAL iteration convenience only — CI never sets it.

> **Golden rule:** to change WHAT CI checks, edit `scripts/ci.sh`. The workflow follows for free. Never add a check only to the YAML (see `/add-ci-gate`).

## The gate, step by step (order matters — fail fast, cheap-before-expensive)

`scripts/ci.sh` runs, in order:

1. **`.codex/hooks/selftest.sh`** — the guardrail meta-test (proves `block-bash`/`secret-scan` still block; prevents a "phantom hook").
2. **`swiftc -typecheck`** the ScreenCaptureKit sidecar (`src-tauri/sysaudio/sysaudio.swift`). macOS-only; skipped with a note if `swiftc` is absent.
3. **`cargo clippy --all-targets -- -D warnings`** (in `src-tauri/`).
4. **`cargo test`** (in `src-tauri/`) — the full test binary.
5. **`cargo audit`** — RUSTSEC advisories (gates on vulnerabilities, not warnings).
6. **`cargo deny check`** — advisories + licenses + bans + sources (`deny.toml`); narrowly gates unmaintained on DIRECT deps.
7. **`cargo build`** (in `src-tauri/`).
8. **`npx ng lint`** then **`npx ng build`** — the Angular gate (incl. the 16 kB per-component style budget).
9. **`e2e-core.sh`** + **`e2e-mix.sh`** — headless audio pipeline (`say` → ffmpeg → whisper → provider/stub → Obsidian `.md`). Skipped when `MURMUR_CI_SKIP_E2E=1`.

`ci.sh` `export`s `MISTRALRS_METAL_PRECOMPILE=0` up front (defers Metal-shader compile to first runtime use — needed on a Command-Line-Tools-only Mac and harmless elsewhere).

## Run it

```bash
source "$HOME/.cargo/env"
cd /Users/jakubgawronski/Projects/meetnotes

# Full gate (what a release must pass; incl. the heavy E2E + model download):
bash scripts/ci.sh

# The PR-gate subset (exactly what the GitHub `gate` job runs — no E2E):
MURMUR_CI_SKIP_E2E=1 bash scripts/ci.sh
```

> The **inner iterate loop is NOT ci.sh** — it's `(cd src-tauri && cargo test --lib)` (fast, ~lib tests). Run the full `ci.sh` once before shipping / to reproduce CI, not on every edit.

## Reproduce a red CI run locally (the #1 job)

1. **Use the SAME command CI used.** CI gate red? → full `bash scripts/ci.sh` (that is what every PR runs since the release-parity change). For a fast local subset while iterating: `MURMUR_CI_SKIP_E2E=1 bash scripts/ci.sh`. Matching the command rules out "works on my machine" drift.
2. **Read the failing step, not the summary.** ci.sh prints a `── <step> ──` banner before each; the first non-zero exit stops it (`set -euo pipefail`). Find the last banner — that's the culprit.
3. **Pull the cloud log** when it's CI-only:
   ```bash
   gh run list -R murmur-io/murmur --workflow CI -L 5
   gh run view <run-id> -R murmur-io/murmur --log-failed
   ```
4. **Classify:** real failure (code/test/lint) vs environment (missing `brew` tool, cold cache, model download) vs flake (there are 2 known pre-existing parallel-test flakes — see the audio-echo shipping notes; re-run to confirm before "fixing").

## Gotchas (repo-specific — each is real)

- **Cold builds are slow, on purpose.** The mistralrs/candle/tokenizers ML tree is ALWAYS compiled (feature gates removed) → the first CI build (or after a deps bump / cache miss) pulls hundreds of MB. `Swatinem/rust-cache` keeps the incremental loop fast. Don't "fix" a slow cold build by ripping out the ML deps.
- **`cargo clippy --all-targets` is BLOCKED as a bare command** by `.codex/hooks/block-bash.sh` (it thrashes the openssl/sqlcipher profile and times out in the iterate loop). It runs fine INSIDE `ci.sh` (allowed as `bash scripts/ci.sh`). To reproduce just clippy, run the whole `ci.sh` — don't call the raw clippy line.
- **The E2E is heavy + host-specific.** `e2e-core.sh` downloads `ggml-base.en.bin` (~142 MB) and needs `say` + ffmpeg; the provider step falls back to a **stub note** when no `claude` CLI is present (so it passes in CI). In CI the model is `actions/cache`d and ffmpeg brew-installed, so the per-PR full gate stays affordable; locally, skip it while iterating with `MURMUR_CI_SKIP_E2E=1`.
- **macOS-only steps.** `swiftc`/ScreenCaptureKit, whisper Metal, `say`, the universal build — a Linux runner would silently skip them. CI must stay on `macos-14`.
- **`cargo audit`/`cargo deny` self-install** (via `cargo install`) if absent — slow. In CI they're pre-installed prebuilt (`taiki-e/install-action`); locally, the first run compiles them once.
- **What CI can't prove — say so.** Real mic, live ScreenCaptureKit, Touch ID, lock-at-rest, screen-share auto-relock, notarization all need a **signed build on a real Mac**. A green gate is not evidence for them.

## Where things live

| Thing | Path |
| --- | --- |
| The gate (source of truth) | `scripts/ci.sh` |
| Headless E2E | `scripts/e2e-core.sh`, `scripts/e2e-mix.sh` |
| GitHub Actions wrapper | `.github/workflows/ci.yml` |
| Guardrail hooks + meta-test | `.codex/hooks/` (`selftest.sh`, `block-bash.sh`, `secret-scan.sh`) |
| Supply-chain policy | `deny.toml` |
| Pinned Rust toolchain | `rust-toolchain.toml` (1.96.0 + clippy/rustfmt) |
| The CI owner-agent | `.codex/agents/ci-cd-engineer.md` |

Adding a step → `/add-ci-gate`. Authoring/altering the workflow → `/github-actions`. Cutting a release (CD) → `/release-murmur` (not this skill).
