#!/usr/bin/env bash
# Deterministic PR evidence for performance-sensitive Murmur paths.
#
# This deliberately gates bounded-memory and lifecycle invariants, not noisy
# wall-clock/RSS values from a shared CI VM. Full physical-footprint and Metal
# evidence stays a controlled signed-Mac lane (see measure-recording-ram.sh).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$ROOT"

(
  cd src-tauri
  CARGO_BUILD_JOBS=2 cargo test --lib audio::recorder::tests -- --test-threads=1
  CARGO_BUILD_JOBS=2 cargo test --lib audio::spill::tests -- --test-threads=1
  CARGO_BUILD_JOBS=2 cargo test --lib perf::tests -- --test-threads=1
  CARGO_BUILD_JOBS=2 cargo test --lib thermal::tests -- --test-threads=1
)

bash -n scripts/measure-recording-ram.sh

echo "performance contracts: PASS"
