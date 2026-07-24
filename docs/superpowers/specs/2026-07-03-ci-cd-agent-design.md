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
- **`scripts/ci.sh` order:** remote-enforcement audit → config/hook/harness/meta-eval selftests → swiftc → `clippy --all-targets -D warnings` → Rust tests (client + brain) → `cargo audit` → `cargo deny` → Rust builds → `ng lint` → `ng build` → `e2e-core.sh` + `e2e-mix.sh`.
- **Guardrails (canonical `.agents/harness/hook_guard.py`, adapted byte-identically by Claude/Codex)** the agent must respect: no direct trunk push, no `security`/keychain CLI, no bare resource-heavy Cargo/build commands outside the shared lane, no `codesign --deep`.
- **E2E is heavy + host-specific:** installs a checksum-pinned ~142 MB Whisper model, needs `say`+ffmpeg, and uses an explicit deterministic stub provider by default. A real Claude call requires both the provider selection and the cloud-egress opt-in; CLI presence alone can never trigger egress.
- **Rust pinned to 1.96.0** (`rust-toolchain.toml`, clippy+rustfmt).
- **What CI cannot prove:** real mic, live ScreenCaptureKit, Touch ID, lock-at-rest, screen-share auto-relock, notarization — need a signed build on a real Mac.

## Deliverables

| Artifact | Path | Purpose |
| --- | --- | --- |
| Agent | `.claude/agents/ci-cd-engineer.md` | Owns CI design/maintenance; hard invariants; output contract; CD → release-engineer. |
| Workflow | `.github/workflows/ci.yml` | One exact required status, `gate (full ci.sh — release parity)`, for PRs, weekly schedule and manual dispatch. Every run includes E2E. PRs lend only the ordinary least-privilege Actions token and attest merge scope only; bypass actors and security settings are explicitly monitor-only and require the privileged trusted default-branch audit. SHA-pinned actions, least-privilege permissions, concurrency cancel, and Rust/npm/model caching. |
| Gate toggle | `scripts/ci.sh` | `MURMUR_CI_SKIP_E2E=1` remains a local iteration convenience only. GitHub Actions never sets it, so the required PR gate stays at release parity. |
| Skill | `.claude/skills/ci-maintenance/SKILL.md` | Runbook: every gate step, reproduce-red-locally, all gotchas. |
| Skill | `.claude/skills/add-ci-gate/SKILL.md` | Discipline for adding a check (ci.sh source of truth, order, tool-absent-safe, selftest RED-before-GREEN, workflow parity). |
| Skill | `.claude/skills/github-actions/SKILL.md` | Workflow best-practices tuned to Murmur + SHA-pin recipe + the CD release blueprint (design-only). |
| Discoverability | `CLAUDE.md` | Added the agent + 3 skills to the `.claude/` index. |

## Key invariant

**`scripts/ci.sh` is the single source of truth; GitHub Actions wraps it.** A check is added to ci.sh (not duplicated in YAML); the workflow only carries runner-level prerequisites (a `brew install`, a cache, a prebuilt cargo tool). This makes local↔cloud drift structurally impossible.

## Deferred (honest gaps)

- **Privileged remote-audit credential** — trusted schedule/manual runs require a repository-scoped `MURMUR_REMOTE_AUDIT_TOKEN`. If it is missing or under-scoped, that monitoring run fails closed; PR `PASS_MERGE_SCOPE` deliberately makes no claim about the three admin-only controls.
- **Semantic/generation product quality** — CI gates the deterministic synthetic FTS floor; real-model semantic/rerank and answer-faithfulness bake-offs remain explicit manual evaluations.
- **Cloud CD (auto-notarized release)** — a documented blueprint only; requires the user to explicitly move Developer-ID signing into cloud CI (collides with the keychain-is-interactive rule).
