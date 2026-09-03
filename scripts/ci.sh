#!/usr/bin/env bash
# Local CI gate for MeetNotes — the single command that must stay green.
# Runs: Rust lint + tests + build, the Angular build, and the headless core E2E.
#
# Usage: bash scripts/ci.sh [STAGE]
#   (no arg) / full  → the complete release-parity gate (unchanged behavior).
#   rust             → the Rust lane: control plane, swiftc, clippy/test/build,
#                      supply-chain, and the audio E2E (cargo-run examples).
#   web              → the Web lane: ng lint + ng build + the Playwright UI E2E.
#
# The STAGE selector exists ONLY so GitHub Actions can run the two independent
# lanes as PARALLEL jobs (wall-clock = max, not sum). It changes NOTHING about
# WHAT is checked: every command stays verbatim below, and `full` runs both
# lanes, so `full == rust ∪ web` by construction — no gate can silently drop.
set -euo pipefail
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

STAGE="${1:-full}"
case "$STAGE" in
  full | rust | web) ;;
  *) echo "usage: bash scripts/ci.sh [full|rust|web]" >&2; exit 2 ;;
esac
run_rust() { [ "$STAGE" = "full" ] || [ "$STAGE" = "rust" ]; }
run_web() { [ "$STAGE" = "full" ] || [ "$STAGE" = "web" ]; }

# mistralrs/metal is ALWAYS compiled (the local brain ships by default). Its build script precompiles
# Metal shaders via `xcrun metal`, which needs the FULL Xcode toolchain — this machine has only the
# Command Line Tools, so we defer shader compile to first runtime use. Without this the cargo steps
# below fail at link time. Safe to always set (it only changes WHEN shaders compile, not whether).
export MISTRALRS_METAL_PRECOMPILE=0

# Kept at top level (not inside a lane function): bash traps are global, and this
# helper is only referenced by the swiftc block within rust_gate.
cleanup_swift_src_tests() {
  for artifact in meetnotes-audiocap meetnotes-aeccap; do
    if [ -e "$SWIFT_SRC_TEST_DIR/$artifact" ]; then
      /bin/unlink "$SWIFT_SRC_TEST_DIR/$artifact"
    fi
  done
  /bin/rmdir "$SWIFT_SRC_TEST_DIR"
}

rust_gate() {
  # Quarantine workflow credentials before executing any repository command.
  # These shell locals are not exported; only the selected audit subprocess
  # receives GH_TOKEN below.
  remote_audit_token="${MURMUR_REMOTE_AUDIT_TOKEN:-}"
  public_audit_token="${MURMUR_PUBLIC_REMOTE_AUDIT_TOKEN:-}"
  unset MURMUR_REMOTE_AUDIT_TOKEN MURMUR_PUBLIC_REMOTE_AUDIT_TOKEN GH_TOKEN GITHUB_TOKEN

  # ── Remote merge enforcement is the first network-aware gate. Local runs exercise the evaluator
  #    deterministically. GitHub Actions lends either the ordinary PR token or the dedicated
  #    privileged monitoring credential only to the matching audit command. Privileged mode has no fallback:
  #    a missing dedicated credential is an explicit gate failure. PRs attest only merge scope;
  #    admin-only controls are MONITOR_ONLY and checked by trusted schedule/manual runs. ──
  echo "── development agent remote enforcement audit ──"
  if [ "${MURMUR_CI_LIVE_REMOTE_AUDIT:-0}" = "1" ]; then
    if [ -z "$remote_audit_token" ]; then
      echo "remote audit: MURMUR_REMOTE_AUDIT_TOKEN is required for privileged monitoring" >&2
      exit 1
    fi
    GH_TOKEN="$remote_audit_token" scripts/agent-remote-audit
  elif [ "${MURMUR_CI_PUBLIC_REMOTE_AUDIT:-0}" = "1" ]; then
    if [ -z "$public_audit_token" ]; then
      echo "remote audit: ordinary pull-request token is missing" >&2
      exit 1
    fi
    GH_TOKEN="$public_audit_token" scripts/agent-remote-audit --public
  else
    # Offline evaluator self-test, 0.33 s — leci zawsze. Zniknal guard zmienionych
    # sciezek, ktory istnial tylko po to, zeby gatowac 54 s selftestu harnessu.
    scripts/agent-remote-audit --selftest
    echo "  (live GitHub audit skipped — set MURMUR_CI_PUBLIC_REMOTE_AUDIT=1 or use privileged live mode)"
  fi
  remote_audit_token=
  public_audit_token=
  unset GH_TOKEN GITHUB_TOKEN

  # ── Lustro konfiguracji agentow: .claude/ i .codex/ musza miec identyczne rules+learnings.
  #    Zastapilo 1787 linii config_audit.py; ten sam defekt, 0.02 s. ──
  echo "── mirror .claude/ vs .codex/ ──"
  .agents/h/mirror-check

  echo "── guard hooka Bash: self-test ──"
  python3 .agents/h/guard.py --selftest

  # ── Martwe komendy IPC. `generate_handler!` to jedyny rejestr wejsc IPC, a kazde jest wolalne
  #    z dowolnego JS w webview; komenda, ktorej nie nazywa zadne `invoke`, to czysta powierzchnia
  #    bez korzysci — nie da sie jej przejsc produktem, wiec gnije, a regresja bramki w jej srodku
  #    jest niewidoczna dla wszystkich pozostalych checkow (kompiluje sie Rust, kompiluje sie FE,
  #    nic ich nie lączy). Na trunku 2026-09-03 bylo 16 takich. --self-test pierwszy, bo pilnuje
  #    INSTRUMENTU: honorowanie `rename` (inaczej zywa komenda wyglada na martwa) i odpornosc na
  #    przywabiacz-podciag oraz nazwe w komentarzu. Ulamek sekundy, bez modelu. ──
  echo "── martwe komendy IPC (instrument + gate) ──"
  python3 scripts/check-dead-commands.py --self-test
  python3 scripts/check-dead-commands.py

  # ── Scaffold eval, control arm only. `--mode fake` runs NO model: it replays each task's
  #    recorded good/bad answers through the real graders and asserts good passes and bad fails.
  #    That is what keeps the graders honest — a grader that has lost its teeth accepts both arms
  #    and the live `--mode agent` measurement silently starts reporting success for everything.
  #    Free, deterministic, seconds; the live arm costs model calls and stays out of CI. ──
  echo "── development agent scaffold eval (graders + instrument, no model) ──"
  # --selftest first: it guards the INSTRUMENT (arm-invariant echo filter, non-vacuous graders,
  # scaffold actually injected, ERROR excluded from pass counts). --mode fake guards the GRADERS.
  # Neither costs a model call. Without the first, a biased instrument reports plausible numbers.
  python3 eval/agents/runner.py --selftest
  python3 eval/agents/runner.py --mode fake

  # ── User-facing vocabulary. WARN mode: it reports, and it FAILS only on a REGRESSION
  #    above the recorded baseline. The baseline may shrink and never grow, so copy debt
  #    can only be paid down. P5 of the UX program arms `--strict`, which forbids any
  #    user-visible jargon at all; arming it before the long-tail rewrite would just
  #    force a rubber-stamp allowlist, which is the failure mode this design avoids. ──
  echo "── user-facing vocabulary (warn + regression gate) ──"
  if command -v node >/dev/null 2>&1; then
    node scripts/check-vocabulary.mjs
  else
    echo "  (skipped — node not on PATH)"
  fi

  echo "── swiftc: system-audio sidecar typecheck ──"
  if command -v swiftc >/dev/null 2>&1; then
    swiftc -typecheck src-tauri/sysaudio/sysaudio.swift \
      -framework ScreenCaptureKit -framework AVFoundation

    echo "── swift capture helpers: TCC-free SRC executable self-tests ──"
    SWIFT_SRC_TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/murmur-swift-src.XXXXXX")"
    trap cleanup_swift_src_tests EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    SWIFT_HOST_ARCH="$(uname -m)"
    case "$SWIFT_HOST_ARCH" in
      arm64 | x86_64) ;;
      *)
        echo "unsupported macOS Swift host architecture: $SWIFT_HOST_ARCH" >&2
        exit 1
        ;;
    esac

    swiftc -O -target "${SWIFT_HOST_ARCH}-apple-macos14.4" \
      -o "$SWIFT_SRC_TEST_DIR/meetnotes-audiocap" \
      src-tauri/audiocap/audiocap.swift \
      -framework AVFoundation -framework CoreAudio -framework Foundation
    "$SWIFT_SRC_TEST_DIR/meetnotes-audiocap" --self-test-src

    swiftc -O -target "${SWIFT_HOST_ARCH}-apple-macos13.4" \
      -o "$SWIFT_SRC_TEST_DIR/meetnotes-aeccap" \
      src-tauri/aeccap/aeccap.swift \
      -framework AVFoundation -framework Foundation
    "$SWIFT_SRC_TEST_DIR/meetnotes-aeccap" --self-test-src

    cleanup_swift_src_tests
    trap - EXIT HUP INT TERM
  else
    echo "  (swiftc not found — skipping Swift sidecar typecheck + capture SRC self-tests)"
  fi

  echo "── cargo clippy (deny warnings) ──"
  ( cd src-tauri && cargo clippy --all-targets -- -D warnings )

  echo "── cargo test ──"
  # The normal test set includes the deterministic synthetic FTS floor. Semantic/reranked and
  # note-generation quality remain manual: they require real local models and/or a labeled vault.
  # nextest parallelizes the test RUN (the slow slice once compiled); it does not run doctests, and
  # this crate has none to lose. Fall back to `cargo test` where nextest is not installed (dev Macs).
  if command -v cargo-nextest >/dev/null 2>&1; then
    ( cd src-tauri && cargo nextest run --no-fail-fast )
  else
    ( cd src-tauri && cargo test --quiet )
  fi

  echo "── murmur-brain sidecar: clippy + tests ──"
  cargo clippy -p murmur-brain --all-targets -- -D warnings
  cargo test -p murmur-brain --quiet

  # ── Supply-chain gates (D11/F5): advisories + license/ban/source policy, BEFORE the build. ──
  echo "── cargo audit (RUSTSEC advisories) ──"
  # Outside the agent sandbox and the SHA-pinned GitHub install step, ALWAYS
  # (re)install, never merely "if missing" — local caches can otherwise retain
  # whatever cargo-audit binary was built months ago indefinitely. That
  # defeats the entire point of the tool: a stale binary can't parse newer advisory-db entries
  # (hit 2026-07-12 — RUSTSEC-2026-0073 uses `cvss = "CVSS:4.0/…"`, which an old cargo-audit's
  # RUSTSEC-parser rejects as "unsupported CVSS version: 4.0", failing CI on an unrelated PR).
  # `--force` is required, not optional: without it, `cargo install` errors "binary already
  # exists in destination" the instant a rust-cache restore leaves a binary on disk whose
  # install-tracking metadata (~/.cargo/.crates2.json) doesn't fully agree with it (observed on
  # CI: it re-downloaded the crate, then refused to place the binary over the existing file) —
  # so a bare `cargo install --locked` is NOT reliably a no-op across a restored cache; `--force`
  # makes the outcome deterministic (always freshly placed) regardless of that metadata's state.
  if [ "${MURMUR_HARNESS:-0}" = "1" ] || [ "${MURMUR_CI_TOOLS_PREINSTALLED:-0}" = "1" ]; then
    # Harness checks are intentionally network-denied. Setup/fetching belongs
    # outside the evidence boundary; inside it we require the pinned local tool
    # and cached advisory DB or fail closed.
    command -v cargo-audit >/dev/null || {
      echo "cargo-audit is required by the preinstalled-tool policy" >&2
      exit 1
    }
  else
    echo "  installing/updating cargo-audit…"
    cargo install --locked --force cargo-audit
  fi
  # Gate on actual VULNERABILITIES (default behavior: non-zero exit on any RUSTSEC vulnerability).
  # We deliberately do NOT pass `--deny warnings`: the Tauri framework pulls in transitive crates
  # with unmaintained/unsound *warnings* (unic-ucd-*, glib) that we cannot fix here and that are not
  # exploitable in this app. The maintenance dimension is still gated — narrowly, on our DIRECT deps
  # — by `cargo deny check` below (`advisories.unmaintained = "workspace"`).
  # The cargo lock moved to the WORKSPACE ROOT (`./Cargo.lock`) when the brain-sidecar extraction made
  # this a virtual workspace; cargo-audit reads the lock from the cwd and does NOT walk up to find it,
  # so point it at the root lock explicitly (this now also covers the `crates/murmur-brain` member).
  if [ "${MURMUR_HARNESS:-0}" = "1" ]; then
    cargo audit --no-fetch --db "${CARGO_HOME}/advisory-db" --file Cargo.lock
  else
    cargo audit --file Cargo.lock
  fi

  echo "── cargo deny check (advisories, licenses, bans, sources) ──"
  # Same install rationale as cargo-audit above — same cached-~/.cargo/bin staleness
  # risk, same category of supply-chain tool where "quietly stopped updating" is the failure mode
  # to avoid, not a build-time optimization to chase. Same `--force` reasoning too.
  if [ "${MURMUR_HARNESS:-0}" = "1" ] || [ "${MURMUR_CI_TOOLS_PREINSTALLED:-0}" = "1" ]; then
    command -v cargo-deny >/dev/null || {
      echo "cargo-deny is required by the preinstalled-tool policy" >&2
      exit 1
    }
  else
    echo "  installing/updating cargo-deny…"
    cargo install --locked --force cargo-deny
  fi
  if [ "${MURMUR_HARNESS:-0}" = "1" ]; then
    cargo deny --offline --manifest-path src-tauri/Cargo.toml check
  else
    cargo deny --manifest-path src-tauri/Cargo.toml check
  fi

  echo "── cargo build ──"
  ( cd src-tauri && cargo build )
  cargo build -p murmur-brain

  # An unchanged tree must rebuild NOTHING. Guards the class that cost 18.6 s on EVERY cargo
  # command until 2026-08-31 (a `cargo:rerun-if-changed` in build.rs on a path that does not
  # exist = permanent staleness). Runs right after `cargo build`, so the warm-up is already paid;
  # the check itself is sub-second on a healthy tree. Rationale + diagnosis live in the script.
  echo "── incremental no-op (nothing may recompile on an unchanged tree) ──"
  python3 scripts/incremental-noop-check
}

web_gate() {
  echo "── ng lint ──"
  npx ng lint

  echo "── ng build ──"
  npx ng build
}

playwright_e2e() {
  echo "── Playwright UI E2E (fresh server, Chromium + WebKit) ──"
  # workers=2 (not 1): specs are page-side mocked and hermetic (private port, no server reuse, no
  # shared state), so spec-file-level parallelism is safe. macos-14 has 3 cores; 2 keeps contention
  # mild against the 30s per-test timeout with retries:0. Do NOT go to 3 without config retries:1.
  #
  # MURMUR_E2E_SHARD ("i/N") rozbija suite na rownolegle joby CI. Bez niego lecimy calosc, wiec
  # `bash scripts/ci.sh web` odpalone recznie nadal jest PELNA bramka web — shardowanie jest
  # wylacznie sposobem, w jaki workflow rozklada ta sama prace na wiecej runnerow.
  if [ -n "${MURMUR_E2E_SHARD:-}" ]; then
    echo "  shard ${MURMUR_E2E_SHARD}"
    npm run test:e2e -- --workers=2 --shard="${MURMUR_E2E_SHARD}"
  else
    npm run test:e2e -- --workers=2
  fi
}

audio_e2e() {
  echo "── headless core E2E (say → ffmpeg → Whisper → provider → Obsidian) ──"
  bash scripts/e2e-core.sh

  echo "── headless mixing E2E (mic + system → mixed transcript, both sides) ──"
  bash scripts/e2e-mix.sh
}

if run_rust; then rust_gate; fi
if run_web; then web_gate; fi

# ── Runtime/E2E lane. Playwright is hermetic per invocation (private port, no server reuse) and
#    exercises Chromium + WebKit over synthetic Tauri IPC. Audio E2E is host-specific: it needs
#    `say` (macOS TTS), ffmpeg, a ~142 MB whisper model (e2e-core.sh) and the cargo-run example — and
#    cloud PR runners have no signed provider. The E2E provider is a deterministic no-egress stub by
#    construction; the example contains no cloud-provider branch. `MURMUR_CI_SKIP_E2E=1` is a
#    local-iteration escape hatch that skips ONLY these runtime/E2E steps (everything above still
#    runs). GitHub Actions never sets it: every PR, scheduled, and manually dispatched run executes
#    this complete release-parity gate. ──
if [ "${MURMUR_CI_SKIP_E2E:-0}" = "1" ]; then
  echo "── headless E2E: SKIPPED for local iteration (MURMUR_CI_SKIP_E2E=1) — GitHub CI always runs the full gate ──"
else
  if run_web; then playwright_e2e; fi
  if run_rust; then audio_e2e; fi
fi

echo
echo "✅ CI: all gates green (stage: $STAGE)"
