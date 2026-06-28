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
( cd src-tauri && cargo test --quiet )

# ── Supply-chain gates (D11/F5): advisories + license/ban/source policy, BEFORE the build. ──
echo "── cargo audit (RUSTSEC advisories) ──"
if ! command -v cargo-audit >/dev/null 2>&1; then
  echo "  cargo-audit not found — installing…"
  cargo install --locked cargo-audit
fi
# Gate on actual VULNERABILITIES (default behavior: non-zero exit on any RUSTSEC vulnerability).
# We deliberately do NOT pass `--deny warnings`: the Tauri framework pulls in transitive crates
# with unmaintained/unsound *warnings* (unic-ucd-*, glib) that we cannot fix here and that are not
# exploitable in this app. The maintenance dimension is still gated — narrowly, on our DIRECT deps
# — by `cargo deny check` below (`advisories.unmaintained = "workspace"`).
( cd src-tauri && cargo audit )

echo "── cargo deny check (advisories, licenses, bans, sources) ──"
if ! command -v cargo-deny >/dev/null 2>&1; then
  echo "  cargo-deny not found — installing…"
  cargo install --locked cargo-deny
fi
cargo deny --manifest-path src-tauri/Cargo.toml check

echo "── cargo build ──"
( cd src-tauri && cargo build )

echo "── ng lint ──"
npx ng lint

echo "── ng build ──"
npx ng build

echo "── headless core E2E (say → ffmpeg → Whisper → provider → Obsidian) ──"
bash scripts/e2e-core.sh

echo "── headless mixing E2E (mic + system → mixed transcript, both sides) ──"
bash scripts/e2e-mix.sh

echo
echo "✅ CI: all gates green"
