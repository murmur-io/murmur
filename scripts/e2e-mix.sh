#!/usr/bin/env bash
# Phase 2 mixing E2E (headless): two speakers — a "mic" track and a "system" track at a
# DIFFERENT sample rate — are read, resampled to 16 kHz, mixed, and transcribed; we assert
# BOTH sides appear in the transcript. This verifies the whole system-audio pipeline
# (read_wav_mono → resample_to_16k → mix → transcribe) EXCEPT the live ScreenCaptureKit
# capture syscall, which needs a real desktop + the Screen Recording permission.
set -euo pipefail
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true
REPO="$(cd "$(dirname "$0")/.." && pwd)"

MODELS_DIR="${MURMUR_WHISPER_MODELS_DIR:-$HOME/Library/Application Support/MeetNotes/models}"
MODEL="$MODELS_DIR/ggml-base.en.bin"
bash "$REPO/scripts/ensure-whisper-model.sh"

# Mixing validates the local audio/transcription path with the example's
# deterministic no-egress summarizer stub.
export MURMUR_E2E_NO_EGRESS=1

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
MIC="$WORK/mic.wav"; SYS="$WORK/sys.wav"; VAULT="$WORK/vault"; mkdir -p "$VAULT"

# Two non-overlapping utterances so Whisper can catch both after the mix:
#   mic  = speech, then trailing silence (apad)            → 16 kHz
#   sys  = leading silence, then speech (adelay 3 s)        → 48 kHz (exercises resample)
say -v Samantha -o "$WORK/mic.aiff" "On our side the budget for Friday is approved." 2>/dev/null \
  || say -o "$WORK/mic.aiff" "On our side the budget for Friday is approved."
say -v Daniel -o "$WORK/sys.aiff" "From the client the contract is now signed." 2>/dev/null \
  || say -o "$WORK/sys.aiff" "From the client the contract is now signed."
ffmpeg -y -loglevel error -i "$WORK/mic.aiff" -af "apad=pad_dur=4" -ar 16000 -ac 1 "$MIC"
ffmpeg -y -loglevel error -i "$WORK/sys.aiff" -af "adelay=3000" -ar 48000 -ac 1 "$SYS"

OUT="$(cd "$REPO/src-tauri" && cargo run --quiet --example e2e_core -- "$MIC" "$MODEL" "$VAULT" "$SYS" 2>&1)"
echo "$OUT" | sed -n '/=== TRANSCRIPT ===/,/==================/p'

fail=0
if printf '%s' "$OUT" | grep -qiE 'budget|friday|approved'; then
  echo "PASS  mic side present in mixed transcript"
else
  echo "FAIL  mic side missing"; fail=1
fi
if printf '%s' "$OUT" | grep -qiE 'client|contract|signed'; then
  echo "PASS  system side present in mixed transcript"
else
  echo "FAIL  system side missing"; fail=1
fi
if [ "$fail" = 0 ]; then
  echo "[e2e-mix] BOTH sides mixed + transcribed ✅"
else
  echo "[e2e-mix] FAILED"; exit 1
fi
