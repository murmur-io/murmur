#!/usr/bin/env bash
# Install or verify the immutable Whisper base.en model used by the audio E2E.
set -euo pipefail

WHISPER_COMMIT="5359861c739e955e79d9a303bcbc70fb988958b1"
WHISPER_SHA256="a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"
MODELS_DIR="${MURMUR_WHISPER_MODELS_DIR:-$HOME/Library/Application Support/MeetNotes/models}"
MODEL="$MODELS_DIR/ggml-base.en.bin"
MODEL_URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/$WHISPER_COMMIT/ggml-base.en.bin"

sha256_of() {
  shasum -a 256 "$1" | awk '{print $1}'
}

mkdir -p "$MODELS_DIR"
if [ -f "$MODEL" ]; then
  actual="$(sha256_of "$MODEL")"
  if [ "$actual" != "$WHISPER_SHA256" ]; then
    echo "Whisper model checksum mismatch: $MODEL" >&2
    echo "expected $WHISPER_SHA256, got $actual" >&2
    echo "Refusing to use or overwrite the unverified cached file." >&2
    exit 1
  fi
  echo "[e2e] verified cached Whisper model: $MODEL"
  exit 0
fi

tmp="$(mktemp "$MODELS_DIR/.ggml-base.en.bin.XXXXXX")"
cleanup() {
  if [ -e "$tmp" ]; then
    /bin/unlink "$tmp"
  fi
}
trap cleanup EXIT
trap 'cleanup; exit 129' HUP
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

echo "[e2e] downloading immutable ggml-base.en.bin (~142 MB, whisper.cpp $WHISPER_COMMIT)..."
curl -fsSL --retry 3 --output "$tmp" "$MODEL_URL"
actual="$(sha256_of "$tmp")"
if [ "$actual" != "$WHISPER_SHA256" ]; then
  echo "Downloaded Whisper model checksum mismatch." >&2
  echo "expected $WHISPER_SHA256, got $actual" >&2
  exit 1
fi
chmod 644 "$tmp"
mv "$tmp" "$MODEL"
trap - EXIT HUP INT TERM
echo "[e2e] installed verified Whisper model: $MODEL"
