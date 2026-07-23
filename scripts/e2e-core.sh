#!/usr/bin/env bash
# Headless end-to-end check of the MeetNotes core pipeline (no mic, no GUI):
#   say (TTS) -> ffmpeg (16 kHz mono WAV) -> Whisper transcription
#   -> deterministic no-egress stub summary -> Obsidian .md export.
# Proves transcription + export work for real on this machine. This runner has
# no cloud-provider branch by construction.
set -euo pipefail
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true

REPO="$(cd "$(dirname "$0")/.." && pwd)"

MODELS_DIR="${MURMUR_WHISPER_MODELS_DIR:-$HOME/Library/Application Support/MeetNotes/models}"
MODEL="$MODELS_DIR/ggml-base.en.bin"
bash "$REPO/scripts/ensure-whisper-model.sh"
echo "[e2e] model: $MODEL ($(du -h "$MODEL" | cut -f1))"

export MURMUR_E2E_NO_EGRESS=1

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
AIFF="$WORK/speech.aiff"
WAV="$WORK/speech.wav"
VAULT="$WORK/vault"
mkdir -p "$VAULT"

SPEECH="Quick sync about the Q3 budget. Decision: we ship the beta on Friday. Action item: Jakub sends the deck to Anna."
echo "[e2e] synthesizing speech with say (Samantha — a clear US English voice)..."
# A clear, high-quality English voice; the default system voice can be a compact/
# non-US voice that Whisper mis-detects as non-English. Fall back if unavailable.
say -v Samantha -o "$AIFF" "$SPEECH" 2>/dev/null || say -o "$AIFF" "$SPEECH"
echo "[e2e] converting to 16 kHz mono WAV with ffmpeg..."
ffmpeg -y -loglevel error -i "$AIFF" -ar 16000 -ac 1 "$WAV"

echo "[e2e] running pipeline (cargo run --example e2e_core)..."
OUT="$(cd "$REPO/src-tauri" && cargo run --quiet --example e2e_core -- "$WAV" "$MODEL" "$VAULT" 2>&1)"
echo "$OUT"

echo "[e2e] === ASSERTIONS ==="
fail=0
NOTE="$(find "$VAULT" -name '*.md' | head -1 || true)"
if [ -n "$NOTE" ] && [ -f "$NOTE" ]; then
  echo "PASS  note written: $NOTE"
else
  echo "FAIL  no note written"; fail=1
fi
if [ -n "$NOTE" ] && [ -f "$NOTE" ] && head -1 "$NOTE" | grep -q '^---'; then
  echo "PASS  note starts with YAML front-matter"
else
  echo "FAIL  note missing front-matter"; fail=1
fi
if printf '%s' "$OUT" | grep -qiE 'budget|friday|deck|beta'; then
  echo "PASS  transcript contains an expected keyword (whisper works)"
else
  echo "FAIL  transcript missing expected keywords"; fail=1
fi
if printf '%s' "$OUT" | grep -q "provider mode: deterministic-stub (no egress)"; then
  echo "PASS  provider mode is deterministic and no-egress"
else
  echo "FAIL  deterministic no-egress provider mode was not reported"; fail=1
fi

if [ "$fail" = "0" ]; then
  echo "[e2e] ALL ASSERTIONS PASSED"
else
  echo "[e2e] ASSERTIONS FAILED"; exit 1
fi
