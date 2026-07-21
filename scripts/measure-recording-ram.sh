#!/usr/bin/env bash
# measure-recording-ram.sh — classify Murmur's recording-time RAM as floor / leak / Stop-peak.
#
# WHY: static code analysis (2026-07-21) put the confirmed steady-state main-process floor at
# ~5-8 GB, which UNDERSHOOTS the observed 14.38 GB (main) + 3.59 GB (meetnotes-brain sidecar).
# The gap is one of three things a code read cannot tell apart:
#   • FLOOR      — RSS climbs early then PLATEAUS  → large-but-bounded residency (whisper large-v3 + audio + candle)
#   • LEAK       — RSS climbs MONOTONICALLY the whole meeting → candle Metal leak (huggingface/candle#2271) or a live-loop grower
#   • STOP-PEAK  — RSS is flat, then SPIKES right when you hit Stop → post-Stop reindex + diarizer + AEC co-residency
# This script samples both PIDs (+ system memory pressure) over a real recording so the shape is unambiguous.
#
# USAGE:
#   bash scripts/measure-recording-ram.sh                 # sample every 5s until Ctrl-C
#   INTERVAL=10 bash scripts/measure-recording-ram.sh     # every 10s
#   bash scripts/measure-recording-ram.sh /tmp/run1.log   # tee to a specific log
#
# HOW TO RUN A CLEAN EXPERIMENT:
#   1. Launch Murmur (the signed DMG for a real-world number; dev build inflates RSS).
#   2. Start this script.  3. Record a real meeting for 30-60+ min.  4. Hit Stop, keep sampling ~60s more.
#   5. Ctrl-C — read the SUMMARY (per-PID min/max/delta). A large main-process DELTA = it grew (leak or long audio);
#      a small delta with a high floor = residency; a sharp jump in the last samples = the Stop peak.
#
# GPU/thermal (needs sudo, run SEPARATELY by you — this script never calls sudo):
#   sudo powermetrics --samplers gpu_power,thermal -i 1000 -n 120
#
# No PII: this logs only process names, PIDs, and memory counts.

set -u

INTERVAL="${INTERVAL:-5}"
LOG="${1:-/tmp/murmur-rss-$(date +%Y%m%d-%H%M%S).log}"

# --- resident set size in MB for a PID (empty if the process is gone) ---
rss_mb() {
  local pid="$1"
  [ -z "$pid" ] && { printf '     -'; return; }
  local kb
  kb="$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ')"
  [ -z "$kb" ] && { printf '     -'; return; }
  awk -v k="$kb" 'BEGIN { printf "%6.0f", k/1024 }'
}

# --- system memory pressure: compressor pool (MB) + swap used (MB) ---
compressor_mb() {
  # vm_stat page size is 16384 on Apple Silicon; read it rather than assume.
  local psize occ
  psize="$(vm_stat 2>/dev/null | awk -F'of ' '/page size/ {gsub(/[^0-9]/,"",$2); print $2; exit}')"
  [ -z "$psize" ] && psize=16384
  occ="$(vm_stat 2>/dev/null | awk -F: '/occupied by compressor/ {gsub(/[^0-9]/,"",$2); print $2; exit}')"
  [ -z "$occ" ] && { printf '   -'; return; }
  awk -v p="$occ" -v s="$psize" 'BEGIN { printf "%5.0f", p*s/1048576 }'
}
swap_mb() {
  sysctl -n vm.swapusage 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="used"){v=$(i+2); gsub(/[A-Za-z]/,"",v); print int(v); exit}}'
}

main_min=99999999; main_max=0; side_min=99999999; side_max=0; samples=0
summary() {
  echo | tee -a "$LOG"
  echo "=================== SUMMARY ===================" | tee -a "$LOG"
  echo "samples: $samples   interval: ${INTERVAL}s   log: $LOG" | tee -a "$LOG"
  if [ "$main_max" -gt 0 ]; then
    awk -v mn="$main_min" -v mx="$main_max" 'BEGIN { printf "Murmur (main):        min %6d MB   max %6d MB   GREW %6d MB\n", mn, mx, mx-mn }' | tee -a "$LOG"
  fi
  if [ "$side_max" -gt 0 ]; then
    awk -v mn="$side_min" -v mx="$side_max" 'BEGIN { printf "meetnotes-brain:      min %6d MB   max %6d MB   GREW %6d MB\n", mn, mx, mx-mn }' | tee -a "$LOG"
  fi
  echo "----------------------------------------------" | tee -a "$LOG"
  echo "READ IT: main GREW large & steadily  -> LEAK (candle#2271) or long-meeting audio buffer (~0.7GB/hr)." | tee -a "$LOG"
  echo "         main GREW mostly in the LAST samples (after Stop) -> Stop-time reindex/diarizer/AEC PEAK." | tee -a "$LOG"
  echo "         main hit its max EARLY then flat -> FLOOR (whisper model + residency); switch large-v3 -> turbo." | tee -a "$LOG"
  echo "         meetnotes-brain flat ~3.6GB the whole time -> KV over-allocation kept hot (with_max_num_seqs fix)." | tee -a "$LOG"
  exit 0
}
trap summary INT TERM

{
  echo "# Murmur recording RAM trace — started $(date '+%Y-%m-%d %H:%M:%S')"
  echo "# time      Murmur_MB  brain_MB   compress_MB  swap_MB   (PIDs resolved each tick; '-' = not running)"
} | tee "$LOG"

while true; do
  MAIN="$(pgrep -x Murmur | head -1)"
  SIDE="$(pgrep -x meetnotes-brain | head -1)"
  m="$(rss_mb "$MAIN")"; s="$(rss_mb "$SIDE")"; c="$(compressor_mb)"; w="$(swap_mb)"
  printf '%s  %s     %s      %6s      %5s\n' "$(date '+%H:%M:%S')" "$m" "$s" "$c" "${w:-0}" | tee -a "$LOG"

  # track min/max for the summary (only when the process is actually up)
  mt="$(printf '%s' "$m" | tr -d ' ')"
  if printf '%s' "$mt" | grep -qE '^[0-9]+$'; then
    [ "$mt" -lt "$main_min" ] && main_min="$mt"; [ "$mt" -gt "$main_max" ] && main_max="$mt"
  fi
  st="$(printf '%s' "$s" | tr -d ' ')"
  if printf '%s' "$st" | grep -qE '^[0-9]+$'; then
    [ "$st" -lt "$side_min" ] && side_min="$st"; [ "$st" -gt "$side_max" ] && side_max="$st"
  fi
  samples=$((samples + 1))
  sleep "$INTERVAL"
done
