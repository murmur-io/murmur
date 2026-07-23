#!/usr/bin/env bash
set -euo pipefail
HOOK_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(CDPATH= cd -- "$HOOK_DIR/../.." && pwd -P)"
exec python3 "$ROOT/.agents/harness/hook_guard.py" block-bash --vendor codex
