#!/usr/bin/env bash
# Local CI gate for MeetNotes — the single command that must stay green.
# Runs: Rust lint + tests + build, the Angular build, and the headless core E2E.
# Usage: bash scripts/ci.sh
set -euo pipefail
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

# mistralrs/metal is ALWAYS compiled (the local brain ships by default). Its build script precompiles
# Metal shaders via `xcrun metal`, which needs the FULL Xcode toolchain — this machine has only the
# Command Line Tools, so we defer shader compile to first runtime use. Without this the cargo steps
# below fail at link time. Safe to always set (it only changes WHEN shaders compile, not whether).
export MISTRALRS_METAL_PRECOMPILE=0

# ── .codex guardrail hooks: self-test (fast, first). Proves the deterministic guardrails still
#    BLOCK what they must (trunk push, security CLI, clippy --all-targets, codesign --deep, staged
#    secrets). This is the meta-test that stops a guardrail from silently going phantom. ──
if [ -x .codex/hooks/selftest.sh ]; then
  echo "── .codex guardrail hooks: self-test ──"
  bash .codex/hooks/selftest.sh
fi

echo "── swiftc: system-audio sidecar typecheck ──"
if command -v swiftc >/dev/null 2>&1; then
  swiftc -typecheck src-tauri/sysaudio/sysaudio.swift \
    -framework ScreenCaptureKit -framework AVFoundation
else
  echo "  (swiftc not found — skipping; system-audio sidecar will not build)"
fi

echo "── cargo clippy (deny warnings) ──"
( cd src-tauri && cargo clippy --all-targets -- -D warnings )

echo "── cargo test ──"
# NOTE: the RAG eval gate (eval::bakeoff #[ignore] runners + eval/results/ artifact) is MANUAL —
# it needs the embed model (and, for the real-vault run, a copied DB) so it is NOT run in CI;
# see docs/RAG-BAKEOFF.md "Synthetic baseline" for the re-run command + merge rule.
( cd src-tauri && cargo test --quiet )

# ── Supply-chain gates (D11/F5): advisories + license/ban/source policy, BEFORE the build. ──
echo "── cargo audit (RUSTSEC advisories) ──"
# ALWAYS (re)install, never "if missing" — CI caches ~/.cargo/bin (Swatinem/rust-cache), so a
# presence check reuses whatever cargo-audit binary was built months ago indefinitely. That
# defeats the entire point of the tool: a stale binary can't parse newer advisory-db entries
# (hit 2026-07-12 — RUSTSEC-2026-0073 uses `cvss = "CVSS:4.0/…"`, which an old cargo-audit's
# RUSTSEC-parser rejects as "unsupported CVSS version: 4.0", failing CI on an unrelated PR).
# `cargo install --locked` is a no-op fetch-and-skip when already current, so this costs
# nothing when the cache is fresh and self-heals when it isn't.
echo "  installing/updating cargo-audit…"
cargo install --locked cargo-audit
# Gate on actual VULNERABILITIES (default behavior: non-zero exit on any RUSTSEC vulnerability).
# We deliberately do NOT pass `--deny warnings`: the Tauri framework pulls in transitive crates
# with unmaintained/unsound *warnings* (unic-ucd-*, glib) that we cannot fix here and that are not
# exploitable in this app. The maintenance dimension is still gated — narrowly, on our DIRECT deps
# — by `cargo deny check` below (`advisories.unmaintained = "workspace"`).
# The cargo lock moved to the WORKSPACE ROOT (`./Cargo.lock`) when the brain-sidecar extraction made
# this a virtual workspace; cargo-audit reads the lock from the cwd and does NOT walk up to find it,
# so point it at the root lock explicitly (this now also covers the `crates/murmur-brain` member).
cargo audit --file Cargo.lock

echo "── cargo deny check (advisories, licenses, bans, sources) ──"
# Same always-(re)install rationale as cargo-audit above — same cached-~/.cargo/bin staleness
# risk, same category of supply-chain tool where "quietly stopped updating" is the failure mode
# to avoid, not a build-time optimization to chase.
echo "  installing/updating cargo-deny…"
cargo install --locked cargo-deny
cargo deny --manifest-path src-tauri/Cargo.toml check

echo "── cargo build ──"
( cd src-tauri && cargo build )

echo "── ng lint ──"
npx ng lint

echo "── ng build ──"
npx ng build

# ── Headless audio E2E. Heavy + host-specific: it needs `say` (macOS TTS), ffmpeg, and a
#    ~142 MB whisper model download (e2e-core.sh) — and cloud PR runners have no signed
#    provider. `MURMUR_CI_SKIP_E2E=1` skips ONLY these two steps (everything above still runs),
#    so the GitHub Actions per-PR `gate` job reuses THIS script as the single source of truth
#    instead of duplicating the command list. Default (unset) = full local behavior, unchanged.
#    The full E2E still runs locally and in the CI `full-gate` job (weekly + on-demand). ──
if [ "${MURMUR_CI_SKIP_E2E:-0}" = "1" ]; then
  echo "── headless E2E: SKIPPED (MURMUR_CI_SKIP_E2E=1) — run the full gate locally or via the CI full-gate job ──"
else
  echo "── headless core E2E (say → ffmpeg → Whisper → provider → Obsidian) ──"
  bash scripts/e2e-core.sh

  echo "── headless mixing E2E (mic + system → mixed transcript, both sides) ──"
  bash scripts/e2e-mix.sh
fi

echo
echo "✅ CI: all gates green"
