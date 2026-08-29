#!/usr/bin/env bash
# Downscale + palette-compress the captured screenshots for the repo.
#
# `capture.mjs` shoots at 2× retina (a 1440-wide viewport becomes a 2880 px PNG),
# which is right for capture and wrong for git: the full set lands around 35 MB.
# This resamples every shot to 1600 px wide and runs pngquant over it, which
# brought the set to roughly a tenth of that with no visible loss at the sizes
# the README and the landing page actually display.
#
# This used to be a copy-paste loop in ./README.md with a hardcoded list of shot
# names — so a NEW shot silently shipped uncompressed. It now globs the directory.
#
#   bash scripts/screenshots/optimize.sh [dir]
#
# Idempotent: re-running on already-optimized files is a no-op beyond a small
# requantization, because sips only ever shrinks toward the target width.
set -euo pipefail

DIR="${1:-$(cd "$(dirname "$0")/../.." && pwd)/docs/screenshots}"
TARGET_W=1600

command -v sips >/dev/null || { echo "sips not found (macOS only)" >&2; exit 1; }
command -v pngquant >/dev/null || { echo "pngquant not found: brew install pngquant" >&2; exit 1; }

shopt -s nullglob
before=0
after=0
count=0
for f in "$DIR"/*.png; do
  sz=$(stat -f%z "$f"); before=$((before + sz))
  w=$(sips -g pixelWidth "$f" | awk '/pixelWidth/{print $2}')
  if [ "${w:-0}" -gt "$TARGET_W" ]; then
    sips --resampleWidth "$TARGET_W" "$f" >/dev/null
  fi
  # `--skip-if-larger` keeps a shot that does not benefit from quantization
  # rather than replacing it with a bigger file.
  pngquant --force --quality=68-92 --strip --skip-if-larger --output "$f" "$f" 2>/dev/null || true
  sz=$(stat -f%z "$f"); after=$((after + sz)); count=$((count + 1))
done

if [ "$count" -eq 0 ]; then
  echo "no PNGs in $DIR" >&2
  exit 1
fi
printf '%d shots: %.1f MB -> %.1f MB\n' "$count" \
  "$(echo "$before" | awk '{print $1/1048576}')" \
  "$(echo "$after" | awk '{print $1/1048576}')"
