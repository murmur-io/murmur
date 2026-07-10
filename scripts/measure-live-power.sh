#!/usr/bin/env bash
# measure-live-power.sh — the ONE-COMMAND live-power baseline for Murmur's LIVE transcription
# loop (T0 of docs/research/2026-07-09-transcription-performance.md).
#
# WHAT IT DOES
#   Runs the headless duty-cycle simulation (`live_duty_cycle_sim_from_env` in
#   src-tauri/src/transcribe/whisper.rs — mirrors live.rs semantics: 3 s ticks, 14 s window,
#   Silero gate with 2-tick hangover, real wall-clock sleeps) three times while `powermetrics`
#   samples CPU/GPU/ANE power, then writes the eval artifact
#   eval/results/live-power-baseline.md. Needs NO running app and touches NO database.
#
# LEGS (one variable ladder, pre-fix worst case → shipped):
#   A  large-v3  gate=0  audio_ctx=0    — the pre-fix worst case (decode every tick, full ctx)
#   B  small     gate=0  audio_ctx=0    — the pre-fix default model
#   C  small     gate=1  audio_ctx=832  — the shipped live loop (VAD gate + right-sized ctx)
#
# USAGE (root is needed for powermetrics ONLY — every cargo invocation runs as $SUDO_USER):
#   sudo MURMUR_DUTY_WAV=/path/to/meeting-16k.wav bash scripts/measure-live-power.sh
#
# ENV KNOBS
#   MURMUR_DUTY_WAV           REQUIRED — a 16 kHz mono WAV of real speech (looped + padded with
#                             MURMUR_DUTY_SILENCE_SECS of silence to ~meeting speech density)
#   MURMUR_DUTY_MINUTES       minutes of simulated wall-clock per leg (default 3)
#   MURMUR_DUTY_SILENCE_SECS  injected silence between WAV loop-plays (default 240)
#   MURMUR_DUTY_SMALL         path to ggml-small.bin        (default: app models dir)
#   MURMUR_DUTY_LARGE         path to ggml-large-v3.bin     (default: app models dir)
#   MURMUR_MODELS_DIR         override the app models dir
#
# NOTES
#   * Run on wall power, screen awake, no other heavy apps — or note the difference.
#   * The Silero VAD model (ggml-silero-v5.1.2.bin) must be in the models dir for leg C
#     (the app downloads it on first Accurate run).

set -euo pipefail

usage() {
  echo "USAGE: sudo MURMUR_DUTY_WAV=/path/to/meeting-16k.wav bash scripts/measure-live-power.sh" >&2
  echo "       (root is for powermetrics only; cargo runs as \$SUDO_USER — see the header)" >&2
}

# ── Preflight: root for powermetrics, a REAL invoking user for cargo ──────────────────────────
if [[ "$(id -u)" -ne 0 ]]; then
  echo "ERROR: powermetrics needs root." >&2
  usage
  exit 1
fi
if [[ -z "${SUDO_USER:-}" || "${SUDO_USER:-}" == "root" ]]; then
  echo "ERROR: SUDO_USER is empty — invoke via 'sudo bash scripts/measure-live-power.sh' from" >&2
  echo "       a normal user shell, never from a root login. Every cargo invocation must run" >&2
  echo "       as the real user (a root-owned target/ dir corrupts the build cache)." >&2
  exit 1
fi
if [[ -z "${MURMUR_DUTY_WAV:-}" ]]; then
  echo "ERROR: MURMUR_DUTY_WAV is required (a 16 kHz mono WAV of real speech)." >&2
  usage
  exit 1
fi

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
USER_HOME="$(dscl . -read "/Users/$SUDO_USER" NFSHomeDirectory 2>/dev/null | awk '{print $2}')"
USER_HOME="${USER_HOME:-/Users/$SUDO_USER}"

MODELS_DIR="${MURMUR_MODELS_DIR:-$USER_HOME/Library/Application Support/MeetNotes/models}"
SMALL_MODEL="${MURMUR_DUTY_SMALL:-$MODELS_DIR/ggml-small.bin}"
LARGE_MODEL="${MURMUR_DUTY_LARGE:-$MODELS_DIR/ggml-large-v3.bin}"
MINUTES="${MURMUR_DUTY_MINUTES:-3}"
SILENCE_SECS="${MURMUR_DUTY_SILENCE_SECS:-240}"

for f in "$MURMUR_DUTY_WAV" "$SMALL_MODEL" "$LARGE_MODEL"; do
  if [[ ! -f "$f" ]]; then
    echo "ERROR: missing file: $f" >&2
    echo "       (models default to the app models dir; override via MURMUR_DUTY_SMALL/_LARGE)" >&2
    exit 1
  fi
done

OUT_DIR="$REPO_DIR/eval/results"
mkdir -p "$OUT_DIR"
STAMP="$(date +%Y%m%d-%H%M%S)"

# ── Crash/ctrl-c safety: powermetrics runs as ROOT in the background with NO sample limit — an
# exit between spawn and kill (any set -e failure, SIGINT/TERM/HUP, closed terminal) would
# otherwise orphan a root sampler writing into the repo FOREVER, and a failed run would leave
# root-owned files inside eval/results/ (breaking the user's later writes there). The EXIT trap
# always kills the current sampler and hands the output dir back to the real user.
PM_PID=""
cleanup() {
  if [[ -n "$PM_PID" ]]; then
    kill "$PM_PID" 2>/dev/null || true
    wait "$PM_PID" 2>/dev/null || true
    PM_PID=""
  fi
  chown -R "$SUDO_USER" "$OUT_DIR" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP # signal → normal exit path → the EXIT trap fires

# Every cargo command runs AS THE REAL USER (-H sets $HOME so ~/.cargo resolves) — the critical
# detail: a single root-owned artifact in src-tauri/target corrupts the user's build cache.
run_as_user() {
  sudo -u "$SUDO_USER" -H env \
    PATH="$USER_HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin" \
    "$@"
}

echo "==> Pre-building the test binary as $SUDO_USER (kept OUT of the measured window)"
run_as_user bash -c "cd '$REPO_DIR/src-tauri' && cargo test --lib --no-run" >/dev/null

run_leg() {
  local name="$1" model="$2" gate="$3" ctx="$4"
  local pm_out="$OUT_DIR/live-power-leg-$name-$STAMP.powermetrics.txt"
  local log_out="$OUT_DIR/live-power-leg-$name-$STAMP.log"

  echo "==> Leg $name: model=$(basename "$model") gate=$gate audio_ctx=$ctx (${MINUTES} min)"
  # PM_PID (script-global) so the EXIT trap can kill a sampler orphaned by a mid-leg failure.
  powermetrics -i 1000 --samplers cpu_power,gpu_power,ane_power,thermal -o "$pm_out" &
  PM_PID=$!
  # Give powermetrics a moment to start sampling before the leg begins.
  sleep 2

  # 2>&1 is REQUIRED: cargo-test harness prints (incl. DUTY_RESULT) can land on stderr.
  run_as_user env \
    MURMUR_DUTY_WAV="$MURMUR_DUTY_WAV" \
    MURMUR_DUTY_MODEL="$model" \
    MURMUR_DUTY_MINUTES="$MINUTES" \
    MURMUR_DUTY_GATE="$gate" \
    MURMUR_DUTY_AUDIO_CTX="$ctx" \
    MURMUR_DUTY_SILENCE_SECS="$SILENCE_SECS" \
    bash -c "cd '$REPO_DIR/src-tauri' && cargo test --lib live_duty_cycle_sim_from_env -- --ignored --nocapture" \
    >"$log_out" 2>&1 || true

  kill "$PM_PID" 2>/dev/null || true
  wait "$PM_PID" 2>/dev/null || true
  PM_PID=""

  local duty
  duty="$(grep -h 'DUTY_RESULT' "$log_out" | tail -1 || true)"
  if [[ -z "$duty" ]]; then
    echo "WARN: leg $name produced no DUTY_RESULT line — see $log_out" >&2
    duty="DUTY_RESULT (missing — see $(basename "$log_out"))"
  fi
  echo "    $duty"
  DUTY_LINES+=("$name|$duty|$pm_out")
}

# Liberal powermetrics parser: field names vary by macOS version ("GPU Power: 123 mW",
# "CPU Power: 456 mW", sometimes combined lines). Averages every "<label>: <n> mW" match for
# the given label; prints n/a when nothing matched (the raw file stays in the artifact).
avg_mw() {
  local file="$1" label="$2"
  awk -v lbl="$label" '
    $0 ~ lbl && / mW/ {
      for (i = 1; i <= NF; i++) {
        if ($i ~ /^[0-9]+(\.[0-9]+)?$/ && $(i+1) ~ /^mW/) { sum += $i; n++ }
      }
    }
    END { if (n > 0) printf "%.0f", sum / n; else printf "n/a" }
  ' "$file" 2>/dev/null || printf "n/a"
}

DUTY_LINES=()
run_leg "A" "$LARGE_MODEL" 0 0
run_leg "B" "$SMALL_MODEL" 0 0
run_leg "C" "$SMALL_MODEL" 1 832

# ── Write the artifact ─────────────────────────────────────────────────────────────────────────
ARTIFACT="$OUT_DIR/live-power-baseline.md"
CHIP="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "unknown")"
COMMIT="$(run_as_user git -C "$REPO_DIR" rev-parse --short HEAD 2>/dev/null || echo "unknown")"

{
  echo "# Live-loop power baseline (duty-cycle simulation)"
  echo
  echo "- Date: $(date '+%Y-%m-%d %H:%M %Z')"
  echo "- Chip: $CHIP"
  echo "- Commit: $COMMIT"
  echo "- Simulated minutes per leg: $MINUTES (WAV looped + ${SILENCE_SECS}s injected silence)"
  echo "- Harness: \`live_duty_cycle_sim_from_env\` (src-tauri/src/transcribe/whisper.rs) +"
  echo "  \`powermetrics -i 1000 --samplers cpu_power,gpu_power,ane_power,thermal\`"
  echo
  echo "| Leg | Config | avg GPU mW | avg CPU mW | DUTY_RESULT |"
  echo "|-----|--------|-----------:|-----------:|-------------|"
  for entry in "${DUTY_LINES[@]}"; do
    IFS='|' read -r name duty pm_file <<<"$entry"
    case "$name" in
      A) cfg="large-v3, gate=0, ctx=0 (pre-fix worst case)" ;;
      B) cfg="small, gate=0, ctx=0 (pre-fix default)" ;;
      C) cfg="small, gate=1, ctx=832 (shipped)" ;;
      *) cfg="?" ;;
    esac
    gpu="$(avg_mw "$pm_file" "GPU Power")"
    cpu="$(avg_mw "$pm_file" "CPU Power")"
    echo "| $name | $cfg | $gpu | $cpu | \`$duty\` |"
  done
  echo
  echo "Raw powermetrics files (per leg, same directory):"
  for entry in "${DUTY_LINES[@]}"; do
    IFS='|' read -r name _duty pm_file <<<"$entry"
    echo "- Leg $name: \`$(basename "$pm_file")\`"
  done
  echo
  echo "## Honest notes"
  echo
  echo "- The audio is a looped fixed WAV + injected silence (synthetic density ~35% speech),"
  echo "  not a live meeting; TTS/recorded speech may VAD-gate slightly differently than a real"
  echo "  far-end mix."
  echo "- Flash attention is ON in ALL legs (baked into \`Transcriber::load\`), so the TRUE"
  echo "  pre-fix baseline was WORSE than legs A/B measured here."
  echo "- powermetrics wattages are Apple's modeled estimates, not shunt measurements."
  echo "- The sim sleeps each tick's remainder, so the idle/busy pattern is real wall-clock;"
  echo "  per-leg duty_pct comes from the DUTY_RESULT line, not from powermetrics."
  echo "- The sim's VAD gate scans a FIXED newest-3 s delta each tick; the shipped live loop"
  echo "  (live.rs \`vad_scan_span_secs\`) scans everything since the last scan + 2 s headroom,"
  echo "  clamped [3, 14] s — at real (decode-stretched) tick spacing it scans MORE audio, so"
  echo "  the sim slightly OVERSTATES gating (fewer boundary speech detections). Hangover"
  echo "  (2 ticks), fail-open on VAD error, 3 s tick, 14 s window and the Fast/greedy decode"
  echo "  profile do match the shipped loop."
} >"$ARTIFACT"

# powermetrics/artifact files are root-owned mid-run; the EXIT trap's cleanup hands $OUT_DIR
# back to $SUDO_USER on EVERY exit path (success, failure, ctrl-c).

echo "==> Artifact: $ARTIFACT"
