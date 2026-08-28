#!/usr/bin/env bash
# Deterministic PR evidence for performance-sensitive Murmur paths.
#
# This deliberately gates bounded-memory and lifecycle invariants, not noisy
# wall-clock/RSS values from a shared CI VM. Full physical-footprint and Metal
# evidence stays a controlled signed-Mac lane (see measure-recording-ram.sh).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

# These four modules are ALREADY EXECUTED by the `rust-lib` check, which runs the whole
# `cargo test --lib` suite. Re-running them here bought nothing and cost a second compile +
# four serialized test processes on every performance-classified diff. What is worth keeping is
# the CONTRACT that the modules still exist, so a rename or deletion cannot silently drop the
# perf coverage `rust-lib` is assumed to carry — that is a static check with no build cost.
for module in \
  "src-tauri/src/audio/recorder.rs" \
  "src-tauri/src/audio/spill.rs" \
  "src-tauri/src/perf.rs" \
  "src-tauri/src/thermal.rs"
do
  if [ ! -f "$module" ]; then
    echo "performance contracts: $module is missing" >&2
    exit 1
  fi
  if ! grep -qE '^\s*mod tests\b|^\s*#\[cfg\(test\)\]' "$module"; then
    echo "performance contracts: $module no longer declares a test module" >&2
    exit 1
  fi
done

bash -n scripts/measure-recording-ram.sh

echo "performance contracts: PASS"
