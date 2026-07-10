#!/usr/bin/env bash
# measure-live-power.sh — the powermetrics A/B protocol for Murmur's LIVE transcription loop
# (T0.2 of docs/research/2026-07-09-transcription-performance.md).
#
# WHAT IT MEASURES
#   CPU / GPU / ANE package power + thermal pressure + per-process energy while Murmur records,
#   sampled every 1 s. This is the ONLY ground truth for the heat work — every modeled number
#   (VAD gate, audio_ctx, live pin, flash-attn, thermal governor) counts only once this script
#   has measured it on a real Mac.
#
# PROTOCOL (run the WHOLE thing twice — leg A vs leg B):
#   1. Prepare a SCRIPTED ~10-minute meeting: play the same fixed PL/EN recording out loud (or
#      a colleague reads the same script) so both legs hear identical audio.
#   2. Leg A: set the live model to `small` (Settings, or config `live_model_pin=small`),
#      start a Murmur recording, then run:
#        sudo scripts/measure-live-power.sh --seconds 600 --out /tmp/live-power-small.txt
#   3. Leg B: repeat the SAME scripted meeting with the live model pinned to `large-v3`
#      (config `live_model_pin=large-v3`, model downloaded):
#        sudo scripts/measure-live-power.sh --seconds 600 --out /tmp/live-power-large-v3.txt
#   4. Pair each leg with Murmur's own per-tick decode telemetry: run the app with
#        RUST_LOG=live_perf=info
#      and keep the emitted `live decode tick` lines (decode_ms / window_s / model) next to the
#      powermetrics file. Commit the two summaries as the eval artifact.
#
# NOTES
#   * powermetrics REQUIRES sudo (it reads SMC/thermal counters).
#   * Run on wall power, screen awake, no other heavy apps — or note the difference.
#   * The same protocol A/Bs any other change: VAD gate on/off (`live_vad_gate`), flash-attn,
#     audio_ctx, quants — one variable per pair of legs.
#
# USAGE
#   sudo scripts/measure-live-power.sh [--seconds N] [--out FILE]
#     --seconds N   sampling duration in seconds (default 600 = the 10-min scripted meeting)
#     --out FILE    output file (default /tmp/murmur-live-power-<timestamp>.txt)
#
# SUMMARIZING (no jq needed — plain grep/awk over the plist-free text output):
#   grep -E "GPU Power|CPU Power|ANE Power" "$OUT" \
#     | awk -F': ' '{sum[$1]+=$2; n[$1]++} END {for (k in sum) printf "%s avg %.0f mW over %d samples\n", k, sum[k]/n[k], n[k]}'
#   grep -E "pressure|Murmur" "$OUT" | sort | uniq -c | head        # thermal levels + process energy lines

set -euo pipefail

SECONDS_TO_SAMPLE=600
OUT="/tmp/murmur-live-power-$(date +%Y%m%d-%H%M%S).txt"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --seconds)
      SECONDS_TO_SAMPLE="${2:?--seconds needs a value}"
      shift 2
      ;;
    --out)
      OUT="${2:?--out needs a path}"
      shift 2
      ;;
    -h|--help)
      sed -n '2,40p' "$0"
      exit 0
      ;;
    *)
      echo "unknown argument: $1 (see --help)" >&2
      exit 2
      ;;
  esac
done

if [[ "$(id -u)" -ne 0 ]]; then
  echo "powermetrics needs root — re-run with sudo." >&2
  exit 1
fi

SAMPLE_COUNT="$SECONDS_TO_SAMPLE" # -i 1000 ms ⇒ one sample per second.

echo "Sampling cpu/gpu/ane power + thermal + per-process energy for ${SECONDS_TO_SAMPLE}s → ${OUT}"
powermetrics \
  -i 1000 \
  -n "$SAMPLE_COUNT" \
  --samplers cpu_power,gpu_power,ane_power,thermal \
  --show-process-energy \
  -o "$OUT"

echo "Done → ${OUT}"
echo "Quick summary:"
grep -E "GPU Power|CPU Power|ANE Power" "$OUT" \
  | awk -F': ' '{sum[$1]+=$2; n[$1]++} END {for (k in sum) printf "  %s avg %.0f mW over %d samples\n", k, sum[k]/n[k], n[k]}' \
  || echo "  (no power lines found — check the samplers on this hardware)"
