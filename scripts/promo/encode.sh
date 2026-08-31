#!/usr/bin/env bash
# Murmur promo-video encoder — STAGE 3 of scripts/promo (see ./README.md).
#
# Turns the PNG sequence rendered by ./render.mjs into the delivered assets:
#
#   promo.mp4        the film — H.264, web-optimised, the one you publish
#   promo-poster.jpg the frame a <video> shows before it plays
#   promo.webm       a VP9 alternate for <source> fallback
#
# Everything here is deliberately conservative: H.264 High profile, yuv420p, and
# +faststart, because the audience is "every browser and every embed", not
# "the newest codec". AV1/HEVC would be smaller and would fail to play somewhere
# that matters.
#
# Usage:  bash scripts/promo/encode.sh [fps] [render-dir] [out-dir]
set -euo pipefail
cd "$(dirname "$0")/../.."

FPS="${1:-60}"
PROMO_DIR="${PROMO_DIR:-.promo}"
IN="${2:-$PROMO_DIR/render}"
OUT="${3:-$PROMO_DIR/out}"

if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "ffmpeg not found — brew install ffmpeg" >&2
  exit 1
fi
if [ ! -d "$IN" ] || [ -z "$(ls -A "$IN" 2>/dev/null)" ]; then
  echo "no rendered frames in $IN — run scripts/promo/render.mjs first" >&2
  exit 1
fi

mkdir -p "$OUT"

# One writer at a time. `mkdir` is atomic on every filesystem we care about, so
# it is the lock. Two concurrent encodes into the same $OUT interleave their
# writes and yield a file that probes correctly — right codec, right duration —
# and decodes to a single repeated frame. That happened; it cost a full re-run
# to notice, because nothing about the artefact looks wrong until you decode it.
LOCK="$OUT/.encode.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
  echo "another encode is already writing to $OUT (remove $LOCK if it is stale)" >&2
  exit 1
fi
trap 'rmdir "$LOCK" 2>/dev/null || true' EXIT

FRAMES=$(ls "$IN" | wc -l | tr -d ' ')
echo "▸ encoding $FRAMES frames @ ${FPS}fps"

# ── The film ────────────────────────────────────────────────────────────────
# -crf 17 is visually lossless for flat UI gradients; the usual 23 bands them.
# -tune stillimage would be wrong (there is real motion) — animation is closer,
# but plain film tuning preserves the type edges best here.
ffmpeg -y -loglevel error -stats \
  -framerate "$FPS" -i "$IN/%06d.png" \
  -c:v libx264 -profile:v high -level 4.2 -preset slow -crf 17 \
  -pix_fmt yuv420p -movflags +faststart \
  -r "$FPS" \
  "$OUT/promo.mp4"

# ── Poster ──────────────────────────────────────────────────────────────────
# A frame from a couple of seconds in: frame 0 is mid-fade on most cuts, and a
# poster that shows a half-faded caption looks like a broken video.
POSTER_FRAME=$(printf "%06d" $((FPS * 2)))
if [ -f "$IN/$POSTER_FRAME.png" ]; then
  ffmpeg -y -loglevel error -i "$IN/$POSTER_FRAME.png" -q:v 3 "$OUT/promo-poster.jpg"
fi

# ── VP9 alternate ───────────────────────────────────────────────────────────
# Two-pass would be smaller; for a <60 s asset the CRF single pass is close
# enough and keeps this script one call per artefact.
ffmpeg -y -loglevel error -stats \
  -framerate "$FPS" -i "$IN/%06d.png" \
  -c:v libvpx-vp9 -crf 30 -b:v 0 -row-mt 1 -pix_fmt yuv420p \
  -r "$FPS" \
  "$OUT/promo.webm"

# ── Web cut ─────────────────────────────────────────────────────────────────
# What actually ships on the landing page. The master above is deliberately fat
# (CRF 17 @ 60 fps) so it survives being re-encoded later; this one is sized to
# be committed to a git repo and served from GitHub Pages, where every megabyte
# is paid for by a visitor. 30 fps is plenty for UI motion, and CRF 23 on a dark
# flat-gradient source is still clean.
ffmpeg -y -loglevel error -stats \
  -framerate "$FPS" -i "$IN/%06d.png" \
  -c:v libx264 -profile:v high -level 4.0 -preset veryslow -crf 23 \
  -pix_fmt yuv420p -movflags +faststart \
  -r 30 \
  "$OUT/promo-web.mp4"

ffmpeg -y -loglevel error -stats \
  -framerate "$FPS" -i "$IN/%06d.png" \
  -c:v libvpx-vp9 -crf 34 -b:v 0 -row-mt 1 -pix_fmt yuv420p \
  -r 30 \
  "$OUT/promo-web.webm"

# ── Verify ──────────────────────────────────────────────────────────────────
# A muxer exits 0 on a file that will not play. Decode the whole thing and count
# the frames: a truncated or interleaved file yields far fewer than it claims,
# which is the one check that would have caught the concurrent-write corruption
# immediately instead of at "why is every frame the end card".
verify() {
  local f="$1" want="$2"
  local got
  got=$(ffmpeg -v error -i "$f" -f null - 2>/dev/null; ffprobe -v error -count_frames \
        -select_streams v:0 -show_entries stream=nb_read_frames -of csv=p=0 "$f" 2>/dev/null)
  got=${got:-0}
  if [ "$got" -lt "$((want * 95 / 100))" ]; then
    echo "✗ $(basename "$f"): decoded $got frames, expected ~$want — the file is corrupt" >&2
    return 1
  fi
  printf "  %-26s %-7s %s frames\n" "$(basename "$f")" "$(du -h "$f" | cut -f1)" "$got"
}

echo
verify "$OUT/promo.mp4"      "$FRAMES"
verify "$OUT/promo.webm"     "$FRAMES"
verify "$OUT/promo-web.mp4"  "$((FRAMES * 30 / FPS))"
verify "$OUT/promo-web.webm" "$((FRAMES * 30 / FPS))"
[ -f "$OUT/promo-poster.jpg" ] && printf "  %-26s %s\n" "promo-poster.jpg" "$(du -h "$OUT/promo-poster.jpg" | cut -f1)"
echo "✓ $OUT"
