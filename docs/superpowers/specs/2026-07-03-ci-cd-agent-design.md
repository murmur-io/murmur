# CI/CD agent + best-practice CI skills — design

**Date:** 2026-07-03
**Status:** implemented
**Author:** QueaT

## Goal

Give Murmur a dedicated **CI/CD agent** that designs and maintains CI, plus **best-practice skills tuned to this repo**. Before this, the repo had no cloud CI at all — `.github/` did not exist; "CI" was the local `scripts/ci.sh` gate only.

## Scope decisions (confirmed with the user)

1. **CI scope:** stand up a real **GitHub Actions macOS PR-gate** that WRAPS `scripts/ci.sh` (ci.sh stays the single source of truth). Not: reimplement the gate in YAML.
2. **CI/CD boundary:** **CI-focused.** The notarized release/CD (sign → notarize → staple → publish) stays with the existing `release-engineer` agent / `/release-murmur` skill — it needs interactive keychain ops that a hard repo rule forbids in the agent shell. The CI/CD agent may DESIGN a release-workflow blueprint but never signs/notarizes.
3. **Skills:** three — `ci-maintenance`, `add-ci-gate`, `github-actions`.

## Repo realities that shaped the design (code-verified)

- **macOS-only build steps:** `swiftc`/ScreenCaptureKit sidecar typecheck, whisper.cpp Metal, `say`/ffmpeg E2E, universal build → CI must run on `macos-14` (Apple Silicon), never Linux.
- **Heavy always-compiled ML tree** (mistralrs/candle/tokenizers — feature gates removed) → cold builds pull hundreds of MB; aggressive caching is essential. `MISTRALRS_METAL_PRECOMPILE=0` defers Metal-shader compile.
- **`scripts/ci.sh` order:** hooks selftest → swiftc → `clippy --all-targets -D warnings` → `cargo test` → `cargo audit` → `cargo deny` → `cargo build` → `ng lint` → `ng build` → `e2e-core.sh` + `e2e-mix.sh`.
- **Guardrails (`.claude/hooks/block-bash.sh`)** the agent must respect: no direct trunk push, no `security`/keychain CLI, no bare `cargo clippy --all-targets` (it's fine inside `bash scripts/ci.sh`), no `codesign --deep`.
- **E2E is heavy + host-specific:** downloads a ~142 MB whisper model, needs `say`+ffmpeg, provider falls back to a stub note when no `claude` CLI (so it passes in CI).
- **Rust pinned to 1.96.0** (`rust-toolchain.toml`, clippy+rustfmt).
- **What CI cannot prove:** real mic, live ScreenCaptureKit, Touch ID, lock-at-rest, screen-share auto-relock, notarization — need a signed build on a real Mac.

## Deliverables

| Artifact | Path | Purpose |
| --- | --- | --- |
| Agent | `.claude/agents/ci-cd-engineer.md` | Owns CI design/maintenance; hard invariants; output contract; CD → release-engineer. |
| Workflow | `.github/workflows/ci.yml` | macOS PR-gate wrapping `ci.sh`. `gate` job (per-PR, `MURMUR_CI_SKIP_E2E=1`) + `full-gate` job (weekly `schedule` + on-demand `workflow_dispatch`, full incl. E2E). SHA-pinned actions, least-priv perms, concurrency cancel, rust/npm/model caching. |
| Gate toggle | `scripts/ci.sh` | Added guarded `MURMUR_CI_SKIP_E2E=1` (additive, default off → local behavior unchanged) so the per-PR job reuses ci.sh instead of duplicating its command list. |
| Skill | `.claude/skills/ci-maintenance/SKILL.md` | Runbook: every gate step, reproduce-red-locally, all gotchas. |
| Skill | `.claude/skills/add-ci-gate/SKILL.md` | Discipline for adding a check (ci.sh source of truth, order, tool-absent-safe, selftest RED-before-GREEN, workflow parity). |
| Skill | `.claude/skills/github-actions/SKILL.md` | Workflow best-practices tuned to Murmur + SHA-pin recipe + the CD release blueprint (design-only). |
| Discoverability | `CLAUDE.md` | Added the agent + 3 skills to the `.claude/` index. |

## Key invariant

**`scripts/ci.sh` is the single source of truth; GitHub Actions wraps it.** A check is added to ci.sh (not duplicated in YAML); the workflow only carries runner-level prerequisites (a `brew install`, a cache, a prebuilt cargo tool). This makes local↔cloud drift structurally impossible.

## Deferred (honest gaps)

- **E2E-in-CI as a per-PR gate** — left as the weekly/on-demand `full-gate` job; making it per-PR needs a provider-auth strategy + accepted model-download cost.
- **Cloud CD (auto-notarized release)** — a documented blueprint only; requires the user to explicitly move Developer-ID signing into cloud CI (collides with the keychain-is-interactive rule).
- **First real cloud run** — the workflow is committed but a live GitHub Actions run has not yet been triggered/observed; first PR into `murmur` (or a manual `workflow_dispatch`) is the real proof.
