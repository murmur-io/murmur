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

# ── Development-agent control plane: fail before paying for any product build. The audit checks
#    both Claude and Codex wiring/config parity; the selftest proves lifecycle, isolation, scope,
#    stale-attestation rejection and the known hook bypasses with fake agents only. ──
echo "── development agent config audit ──"
scripts/agent-config-audit --ci

echo "── development agent hooks: adversarial self-test ──"
bash .codex/hooks/selftest.sh

echo "── development agent harness: deterministic self-test ──"
scripts/agent-harness selftest --ci

echo "── development agent eval harness: deterministic self-test ──"
scripts/agent-harness eval selftest

echo "── development agent remote-policy evaluator: deterministic self-test ──"
# This tests the fail-closed policy logic without network/auth. The live GitHub
# audit is a separate scheduled/operator preflight because harness checks are
# intentionally network-denied and administration reads need explicit authority.
scripts/agent-remote-audit --selftest

echo "── swiftc: system-audio sidecar typecheck ──"
if command -v swiftc >/dev/null 2>&1; then
  swiftc -typecheck src-tauri/sysaudio/sysaudio.swift \
    -framework ScreenCaptureKit -framework AVFoundation

  echo "── swift capture helpers: TCC-free SRC executable self-tests ──"
  SWIFT_SRC_TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/murmur-swift-src.XXXXXX")"
  cleanup_swift_src_tests() {
    for artifact in meetnotes-audiocap meetnotes-aeccap; do
      if [ -e "$SWIFT_SRC_TEST_DIR/$artifact" ]; then
        /bin/unlink "$SWIFT_SRC_TEST_DIR/$artifact"
      fi
    done
    /bin/rmdir "$SWIFT_SRC_TEST_DIR"
  }
  trap cleanup_swift_src_tests EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

  SWIFT_HOST_ARCH="$(uname -m)"
  case "$SWIFT_HOST_ARCH" in
    arm64 | x86_64) ;;
    *)
      echo "unsupported macOS Swift host architecture: $SWIFT_HOST_ARCH" >&2
      exit 1
      ;;
  esac

  swiftc -O -target "${SWIFT_HOST_ARCH}-apple-macos14.4" \
    -o "$SWIFT_SRC_TEST_DIR/meetnotes-audiocap" \
    src-tauri/audiocap/audiocap.swift \
    -framework AVFoundation -framework CoreAudio -framework Foundation
  "$SWIFT_SRC_TEST_DIR/meetnotes-audiocap" --self-test-src

  swiftc -O -target "${SWIFT_HOST_ARCH}-apple-macos13.4" \
    -o "$SWIFT_SRC_TEST_DIR/meetnotes-aeccap" \
    src-tauri/aeccap/aeccap.swift \
    -framework AVFoundation -framework Foundation
  "$SWIFT_SRC_TEST_DIR/meetnotes-aeccap" --self-test-src

  cleanup_swift_src_tests
  trap - EXIT HUP INT TERM
else
  echo "  (swiftc not found — skipping Swift sidecar typecheck + capture SRC self-tests)"
fi

echo "── cargo clippy (deny warnings) ──"
( cd src-tauri && cargo clippy --all-targets -- -D warnings )

echo "── cargo test ──"
# NOTE: the RAG eval gate (eval::bakeoff #[ignore] runners + eval/results/ artifact) is MANUAL —
# it needs the embed model (and, for the real-vault run, a copied DB) so it is NOT run in CI;
# see docs/RAG-BAKEOFF.md "Synthetic baseline" for the re-run command + merge rule.
( cd src-tauri && cargo test --quiet )

echo "── murmur-brain sidecar: clippy + tests ──"
cargo clippy -p murmur-brain --all-targets -- -D warnings
cargo test -p murmur-brain --quiet

# ── Supply-chain gates (D11/F5): advisories + license/ban/source policy, BEFORE the build. ──
echo "── cargo audit (RUSTSEC advisories) ──"
# Outside the agent sandbox and the SHA-pinned GitHub install step, ALWAYS
# (re)install, never merely "if missing" — local caches can otherwise retain
# whatever cargo-audit binary was built months ago indefinitely. That
# defeats the entire point of the tool: a stale binary can't parse newer advisory-db entries
# (hit 2026-07-12 — RUSTSEC-2026-0073 uses `cvss = "CVSS:4.0/…"`, which an old cargo-audit's
# RUSTSEC-parser rejects as "unsupported CVSS version: 4.0", failing CI on an unrelated PR).
# `--force` is required, not optional: without it, `cargo install` errors "binary already
# exists in destination" the instant a rust-cache restore leaves a binary on disk whose
# install-tracking metadata (~/.cargo/.crates2.json) doesn't fully agree with it (observed on
# CI: it re-downloaded the crate, then refused to place the binary over the existing file) —
# so a bare `cargo install --locked` is NOT reliably a no-op across a restored cache; `--force`
# makes the outcome deterministic (always freshly placed) regardless of that metadata's state.
if [ "${MURMUR_HARNESS:-0}" = "1" ] || [ "${MURMUR_CI_TOOLS_PREINSTALLED:-0}" = "1" ]; then
  # Harness checks are intentionally network-denied. Setup/fetching belongs
  # outside the evidence boundary; inside it we require the pinned local tool
  # and cached advisory DB or fail closed.
  command -v cargo-audit >/dev/null || {
    echo "cargo-audit is required by the preinstalled-tool policy" >&2
    exit 1
  }
else
  echo "  installing/updating cargo-audit…"
  cargo install --locked --force cargo-audit
fi
# Gate on actual VULNERABILITIES (default behavior: non-zero exit on any RUSTSEC vulnerability).
# We deliberately do NOT pass `--deny warnings`: the Tauri framework pulls in transitive crates
# with unmaintained/unsound *warnings* (unic-ucd-*, glib) that we cannot fix here and that are not
# exploitable in this app. The maintenance dimension is still gated — narrowly, on our DIRECT deps
# — by `cargo deny check` below (`advisories.unmaintained = "workspace"`).
# The cargo lock moved to the WORKSPACE ROOT (`./Cargo.lock`) when the brain-sidecar extraction made
# this a virtual workspace; cargo-audit reads the lock from the cwd and does NOT walk up to find it,
# so point it at the root lock explicitly (this now also covers the `crates/murmur-brain` member).
if [ "${MURMUR_HARNESS:-0}" = "1" ]; then
  cargo audit --no-fetch --db "${CARGO_HOME}/advisory-db" --file Cargo.lock
else
  cargo audit --file Cargo.lock
fi

echo "── cargo deny check (advisories, licenses, bans, sources) ──"
# Same install rationale as cargo-audit above — same cached-~/.cargo/bin staleness
# risk, same category of supply-chain tool where "quietly stopped updating" is the failure mode
# to avoid, not a build-time optimization to chase. Same `--force` reasoning too.
if [ "${MURMUR_HARNESS:-0}" = "1" ] || [ "${MURMUR_CI_TOOLS_PREINSTALLED:-0}" = "1" ]; then
  command -v cargo-deny >/dev/null || {
    echo "cargo-deny is required by the preinstalled-tool policy" >&2
    exit 1
  }
else
  echo "  installing/updating cargo-deny…"
  cargo install --locked --force cargo-deny
fi
if [ "${MURMUR_HARNESS:-0}" = "1" ]; then
  cargo deny --offline --manifest-path src-tauri/Cargo.toml check
else
  cargo deny --manifest-path src-tauri/Cargo.toml check
fi

echo "── cargo build ──"
( cd src-tauri && cargo build )
cargo build -p murmur-brain

echo "── ng lint ──"
npx ng lint

echo "── ng build ──"
npx ng build

# ── Runtime/E2E lane. Playwright is hermetic per invocation (private port, no server reuse) and
#    exercises Chromium + WebKit over synthetic Tauri IPC. Audio E2E is host-specific: it needs
#    `say` (macOS TTS), ffmpeg, and a
#    ~142 MB whisper model download (e2e-core.sh) — and cloud PR runners have no signed
#    provider. `MURMUR_CI_SKIP_E2E=1` skips ONLY these three runtime/E2E steps (everything above still runs),
#    so the GitHub Actions per-PR `gate` job reuses THIS script as the single source of truth
#    instead of duplicating the command list. Default (unset) = full local behavior, unchanged.
#    The full E2E runs locally and in the CI gate on PRs, weekly, and on demand. ──
if [ "${MURMUR_CI_SKIP_E2E:-0}" = "1" ]; then
  echo "── headless E2E: SKIPPED (MURMUR_CI_SKIP_E2E=1) — run the full gate locally or via the CI full-gate job ──"
else
  echo "── Playwright UI E2E (fresh server, Chromium + WebKit) ──"
  npm run test:e2e -- --workers=1

  echo "── headless core E2E (say → ffmpeg → Whisper → provider → Obsidian) ──"
  bash scripts/e2e-core.sh

  echo "── headless mixing E2E (mic + system → mixed transcript, both sides) ──"
  bash scripts/e2e-mix.sh
fi

echo
echo "✅ CI: all gates green"
