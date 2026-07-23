#!/usr/bin/env bash
# measure-recording-ram.sh — time-series Murmur's recording-time memory and Metal footprint.
#
# WHY: Activity Monitor reported 14.38 GB for Murmur + 3.59 GB for the brain helper. `ps` RSS alone
# misses Metal-owned memory (an e5 soak measured RSS 980→989 MB while physical footprint grew
# 1064→1711 MB), so this traces BOTH memory metrics plus CPU/threads for both processes and system
# memory pressure:
#   • FLOOR      — footprint climbs early then PLATEAUS → large-but-bounded model/Metal residency
#   • LEAK       — footprint keeps a positive slope for the whole meeting → an unbounded live grower
#   • STOP EVENT — a correlated jump is a lead to inspect; by itself it does NOT prove overlap
# Physical footprint is the number to compare with Activity Monitor; RSS stays as a diagnostic.
#
# USAGE:
#   bash scripts/measure-recording-ram.sh                 # sample every 5s until Ctrl-C
#   INTERVAL=10 bash scripts/measure-recording-ram.sh     # every 10s
#   bash scripts/measure-recording-ram.sh /tmp/run1.log   # tee to a specific log
#
# HOW TO RUN A CLEAN EXPERIMENT:
#   1. Launch Murmur (the signed DMG for a real-world number; dev build inflates RSS).
#   2. Start this script.  3. Record a real meeting for 30-60+ min.  4. Hit Stop, keep sampling ~60s more.
#   5. Ctrl-C — read the SUMMARY. Mic capture itself must not create a duration-proportional
#      main-footprint slope; the fixed 32 MiB meeting ring owns a bounded 14s live-history tail
#      and the fsynced generation file owns every older frame.
#
# GPU/thermal (needs sudo, run SEPARATELY by you — this script never calls sudo):
#   sudo powermetrics --samplers gpu_power,thermal -i 1000 -n 120
#
# No PII: this logs only process names, PIDs, and memory counts.

set -u

INTERVAL="${INTERVAL:-5}"
LOG="${1:-/tmp/murmur-memory-$(date +%Y%m%d-%H%M%S).log}"
START_EPOCH="$(date +%s)"

single_pid() {
  local name="$1" override="$2" label="$3" pids count
  if [ -n "$override" ]; then
    printf '%s' "$override"
    return 0
  fi
  pids="$(pgrep -x "$name" 2>/dev/null || true)"
  count="$(printf '%s\n' "$pids" | awk 'NF {n++} END {print n+0}')"
  if [ "$count" -gt 1 ]; then
    echo "ERROR: multiple $label processes found; set MURMUR_PID or BRAIN_PID explicitly." >&2
    return 2
  fi
  printf '%s' "$pids"
}

brain_pid() {
  local override="${BRAIN_PID:-}" pids count
  if [ -n "$override" ]; then
    printf '%s' "$override"
    return 0
  fi
  pids="$(
    {
      pgrep -x murmur-brain 2>/dev/null || true
      pgrep -x meetnotes-brain 2>/dev/null || true
    } | awk 'NF && !seen[$0]++'
  )"
  count="$(printf '%s\n' "$pids" | awk 'NF {n++} END {print n+0}')"
  if [ "$count" -gt 1 ]; then
    echo "ERROR: multiple brain sidecars found; set BRAIN_PID explicitly." >&2
    return 2
  fi
  printf '%s' "$pids"
}

# --- resident set size in MB for a PID (empty if the process is gone) ---
rss_mb() {
  local pid="$1"
  [ -z "$pid" ] && { printf '     -'; return; }
  local kb
  kb="$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ')"
  [ -z "$kb" ] && { printf '     -'; return; }
  awk -v k="$kb" 'BEGIN { printf "%6.0f", k/1024 }'
}

# --- Activity Monitor-style physical footprint in MB (includes Metal-owned allocations) ---
footprint_mb() {
  local pid="$1"
  [ -z "$pid" ] && { printf '     -'; return; }
  local bytes
  bytes="$(/usr/bin/footprint -p "$pid" --noCategories -f bytes 2>/dev/null \
    | awk '/^[[:space:]]*phys_footprint:/ {print $2; exit}')"
  [ -z "$bytes" ] && { printf '     -'; return; }
  awk -v b="$bytes" 'BEGIN { printf "%6.0f", b/1048576 }'
}

cpu_pct() {
  local pid="$1"
  [ -z "$pid" ] && { printf '     -'; return; }
  local value
  value="$(ps -o %cpu= -p "$pid" 2>/dev/null | awk 'NF {print $1; exit}')"
  [ -z "$value" ] && { printf '     -'; return; }
  awk -v v="$value" 'BEGIN { printf "%6.1f", v }'
}

thread_count() {
  local pid="$1"
  [ -z "$pid" ] && { printf '    -'; return; }
  local value
  value="$(ps -o thcount= -p "$pid" 2>/dev/null | awk 'NF {print $1; exit}')"
  [ -z "$value" ] && { printf '    -'; return; }
  awk -v v="$value" 'BEGIN { printf "%5d", v }'
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

main_rss_min=99999999; main_rss_max=0; main_fp_min=99999999; main_fp_max=0
side_rss_min=99999999; side_rss_max=0; side_fp_min=99999999; side_fp_max=0; samples=0
summary() {
  echo | tee -a "$LOG"
  echo "=================== SUMMARY ===================" | tee -a "$LOG"
  echo "samples: $samples   interval: ${INTERVAL}s   log: $LOG" | tee -a "$LOG"
  if [ "$main_fp_max" -gt 0 ]; then
    awk -v rmn="$main_rss_min" -v rmx="$main_rss_max" -v fmn="$main_fp_min" -v fmx="$main_fp_max" \
      'BEGIN { printf "Murmur:       RSS %6d→%6d MB (%+6d)   footprint %6d→%6d MB (%+6d)\n", rmn, rmx, rmx-rmn, fmn, fmx, fmx-fmn }' | tee -a "$LOG"
  fi
  if [ "$side_fp_max" -gt 0 ]; then
    awk -v rmn="$side_rss_min" -v rmx="$side_rss_max" -v fmn="$side_fp_min" -v fmx="$side_fp_max" \
      'BEGIN { printf "murmur-brain:  RSS %6d→%6d MB (%+6d)   footprint %6d→%6d MB (%+6d)\n", rmn, rmx, rmx-rmn, fmn, fmx, fmx-fmn }' | tee -a "$LOG"
  fi
  echo "----------------------------------------------" | tee -a "$LOG"
  echo "READ IT: use footprint, not RSS. Min/max alone does not classify a leak; inspect the time-series." | tee -a "$LOG"
  echo "         Early plateau = bounded model/Metal high-water; correlate jumps with exact Record/Stop logs." | tee -a "$LOG"
  echo "         normal meeting mic RAM is a fixed 32 MiB ring (14s live history at <=384 kHz); older frames live in the fsynced generation file." | tee -a "$LOG"
  echo "         a capture/storage fault must auto-finalize the exact durable prefix; it must never switch to a growing fallback." | tee -a "$LOG"
  echo "         a Stop-correlated jump is evidence to inspect, not proof of which models overlapped." | tee -a "$LOG"
  echo "         judge brain residency against the configured GGUF; a pre-existing child may idle for ~300s." | tee -a "$LOG"
  echo "         with Brain Live OFF, the invariant is: a cold recording must not launch/refresh the child." | tee -a "$LOG"
  exit 0
}
trap summary INT TERM

{
  echo "# Murmur recording RAM trace — started $(date '+%Y-%m-%d %H:%M:%S')"
  echo "# time      elapsed  main_rss main_foot main_cpu main_thr  brain_rss brain_foot brain_cpu brain_thr  compress swap"
  echo "#                    memory MB          %        count     memory MB           %         count      memory MB"
} | tee "$LOG"

while true; do
  MAIN="$(single_pid Murmur "${MURMUR_PID:-}" "Murmur")" || exit 2
  SIDE="$(brain_pid)" || exit 2
  mr="$(rss_mb "$MAIN")"; mf="$(footprint_mb "$MAIN")"
  mc="$(cpu_pct "$MAIN")"; mt="$(thread_count "$MAIN")"
  sr="$(rss_mb "$SIDE")"; sf="$(footprint_mb "$SIDE")"
  sc="$(cpu_pct "$SIDE")"; st="$(thread_count "$SIDE")"
  c="$(compressor_mb)"; w="$(swap_mb)"
  now_epoch="$(date +%s)"
  elapsed="$((now_epoch - START_EPOCH))"
  printf '%s  %7s  %s   %s  %s  %s    %s    %s  %s   %s    %6s  %5s\n' \
    "$(date '+%H:%M:%S')" "$elapsed" "$mr" "$mf" "$mc" "$mt" \
    "$sr" "$sf" "$sc" "$st" "$c" "${w:-0}" | tee -a "$LOG"

  # track min/max for the summary (only when the process is actually up)
  mrt="$(printf '%s' "$mr" | tr -d ' ')"
  if printf '%s' "$mrt" | grep -qE '^[0-9]+$'; then
    [ "$mrt" -lt "$main_rss_min" ] && main_rss_min="$mrt"; [ "$mrt" -gt "$main_rss_max" ] && main_rss_max="$mrt"
  fi
  mft="$(printf '%s' "$mf" | tr -d ' ')"
  if printf '%s' "$mft" | grep -qE '^[0-9]+$'; then
    [ "$mft" -lt "$main_fp_min" ] && main_fp_min="$mft"; [ "$mft" -gt "$main_fp_max" ] && main_fp_max="$mft"
  fi
  srt="$(printf '%s' "$sr" | tr -d ' ')"
  if printf '%s' "$srt" | grep -qE '^[0-9]+$'; then
    [ "$srt" -lt "$side_rss_min" ] && side_rss_min="$srt"; [ "$srt" -gt "$side_rss_max" ] && side_rss_max="$srt"
  fi
  sft="$(printf '%s' "$sf" | tr -d ' ')"
  if printf '%s' "$sft" | grep -qE '^[0-9]+$'; then
    [ "$sft" -lt "$side_fp_min" ] && side_fp_min="$sft"; [ "$sft" -gt "$side_fp_max" ] && side_fp_max="$sft"
  fi
  samples=$((samples + 1))
  sleep "$INTERVAL"
done
