# Rust profiling recipe + the OOM anatomy

Deep reference for `/app-perf-audit`. The Rust/Tauri ruleset is `.claude/rules/rust-tauri.md` (§9 test
loop, §8 no-PII-logs) — this file is the **measurement toolchain** and the **memory/OOM** map. Model
tokens/sec, quant, and KV-cache math are NOT here — that is `ondevice-model-perf` /
`model-perf-engineer`; the two co-own only `thermal.rs` tick-policy + the `mistral.rs` RAM-guard half.
Cite by SYMBOL, not line number (the big files drift).

## The profiling toolchain (a CPU sampler alone lies)

- **CPU + wall (on-CPU cost):** `samply record -- <cmd>` (opens a Firefox-profiler flamegraph in the
  browser), or `cargo instruments -t "Time Profiler"` (needs the full Xcode Instruments — this Mac has
  Command-Line-Tools only, so `samply` is the default; `cargo instruments` is a real-Mac-with-Xcode
  step). Profile the release-ish binary for a real number; a debug build's cost is not representative.
- **THE BLIND SPOT — async / IO / DB-lock / Metal-wait is INVISIBLE to a CPU sampler.** A pure CPU
  sampler shows only threads that are ON-CPU. A task blocked awaiting IO, waiting on the SQLite lock, or
  parked on a Metal completion shows as *nothing* — the flamegraph looks idle while the wall clock
  burns. Murmur is full of exactly these waits (SQLCipher reads, the shared model runtime, `spawn_blocking`
  hops). So ALWAYS pair the sampler with:
  - **`tokio-console`** — live view of every tokio task: which are stalled, blocked-on, or long-poll.
    The app already builds one process-wide multi-thread runtime (`brain_rt` in `reason/mistral.rs`,
    `tauri::async_runtime` for the setup loops); console shows where those tasks wait.
  - **`tracing` spans** — instrument the hot path with `#[tracing::instrument]` / manual spans, `target:`
    + non-PII fields (stage names, counts, durations). The codebase logs this way already
    (`target: "rag"`, `target: "startup"`, `target: "memory"`). A span that shows elapsed wall while the
    sampler shows no CPU = a WAIT, not compute — that is the finding.
- **Reading the result:** if the sampler and the span agree, it's compute — optimize the code. If the
  span shows time the sampler doesn't, it's a wait — the fix is concurrency / avoiding the lock / not
  blocking the runtime (e.g. `spawn_blocking` for a sync model call so the async runtime isn't parked,
  the pattern already used in the `lib.rs` setup loops).

## Loop discipline (binding)

```bash
source "$HOME/.cargo/env"
( cd src-tauri && cargo test --lib )      # the fast Rust loop
```

**NEVER run `cargo clippy --all-targets` in the iterate loop** — it rebuilds the test/bench targets
against the openssl + sqlcipher profile and thrashes/times out. The heavy always-compiled ML tree
(mistralrs/candle) makes a COLD first build slow; let it finish, don't bail. `scripts/ci.sh` (which
DOES run clippy `-D warnings`) is the FINAL gate, run ONCE.

## `[profile.release]` — the block that is MISSING

`src-tauri/Cargo.toml` has **no `[profile.release]` section today** (the sections end at `[features]`;
no `lto`, `codegen-units`, or `strip`). For a shipped desktop binary that leaves steady-state speed and
binary size on the table. If the task is release-perf or binary-size, add it:

```toml
[profile.release]
lto = true            # link-time optimization across crates — real steady-state wins
codegen-units = 1     # one unit = maximum cross-function optimization (slower build, faster binary)
strip = true          # strip symbols — smaller binary
# opt-level = 3 is the release default; keep it.
```

This is **configuration, not a dependency** — no user approval needed (unlike a new crate). It slows the
release build; it does NOT affect the `cargo test --lib` dev loop. Measure binary size + a
representative wall-clock before/after; verify a release build still notarizes (that is
`release-engineer`'s gate — flag it).

## Startup latency (measured at the setup thread)

The one place to add work carefully is `src-tauri/src/lib.rs` `.setup(|app| …)`. What it already does
RIGHT (preserve it):

- `AppState::init()` opens the SQLCipher DB first; on failure → `show_fatal_init_dialog` →
  `std::process::exit(1)` off a worker thread — a **graceful dialog + clean exit, NEVER a panic**
  (the v0.3.0 hard-crash / v0.3.1 fix). Never reintroduce an `init().expect()`/`.unwrap()` on the
  keychain-or-DB-open path.
- Heavy work is DEFERRED: the topic-chunk backfill runs on `tauri::async_runtime::spawn_blocking` and
  early-returns when semantic search is off or the embed model is absent (a default install writes
  nothing). The memory-consolidation loop (`CONSOLIDATION_INTERVAL_SECS`) and brief runner
  (`BRIEF_TICK_SECS`) are detached `spawn` loops whose FIRST tick is a full interval AFTER launch — no
  startup Metal contention. Crash-recovery/salvage (`claim_inflight`, `reap_orphaned_capture_helpers`,
  `spawn_salvage`) runs in a load-bearing order but is cheap/bounded.

**To measure startup:** `tee /tmp/murmur-dev.log` the dev run, and add spans around each setup phase
(DB open, backfill schedule, MCP spawn, window create). A clean boot prints the ng URL (`:1420`) + the
MCP listen line (`:8765`). **The rule for any new startup work: it goes on `spawn_blocking` or a
deferred loop, never inline in `.setup()`, and never anything that can panic the setup thread.**

## The OOM anatomy — why opening a 1h meeting killed the Mac

This is the load-bearing memory story. The kill was NOT one giant allocation — it was **cumulative
residency crossing the swap-death threshold**, tipped over by on-open work. The four layers, and where
each guard lives:

1. **Never-evict model residency.** `reason/mistral.rs` `MODEL_CACHE_CAP = 2` holds up to two GGUF models
   resident and **REFUSES rather than evicts** at the cap (mistralrs has documented drop-leaks, so
   evicting would leak — the deliberate choice is refuse-and-degrade to the deterministic floor). So
   whisper + a light + a heavy model can all be co-resident (multiple GB). *Guard:* the RAM check below,
   gating the NEXT load.
2. **Un-chunked transcript into a local model.** Feeding a whole 1h transcript to an on-device summarizer
   spikes the prefill KV cache + activations. *Guard:* UNIFORM DECIMATION in `summarize/timeline.rs`
   (bounds the on-device input; a cloud provider gets the full transcript — no cap). Never
   head-truncate — the decimated transcript must SPAN the whole meeting.
3. **DOM node count of an un-windowed `@for`.** The full transcript rendered as thousands of DOM turns
   bloats the webview process. *Guard:* the `RENDER_CAP = 80` window in `audio-panel.component.ts`
   (fe-cd-checklist), + a bounded backend read (ipc-payload-patterns).
4. **A MISSING RAM guard on the allocation.** The pre-fix hole: nothing checked free RAM before a
   multi-GB load. *Guard:* the P0.3 RAM guard in `reason/mistral.rs`.

### The RAM guard (fail-open, and the pattern to extend app-wide)

```rust
// reason/mistral.rs — PURE + injectable so it's unit-testable; the OS probe is separate.
fn ram_permits_load(free_bytes: Option<u64>, model_disk_bytes: u64) -> bool {
    if model_disk_bytes < MODEL_RAM_GUARD_MIN_DISK_BYTES { return true; } // tiny model — never the driver
    let Some(free) = free_bytes else { return true; };                    // probe broke → FAIL OPEN
    let needed = model_disk_bytes.saturating_mul(MODEL_RAM_HEADROOM_NUM) / MODEL_RAM_HEADROOM_DEN;
    free >= needed                                                        // headroom 3/2 of on-disk
}
```

Key properties, and why they generalize:

- **FAILS OPEN.** `free_bytes = None` (the `vm_stat` probe in `available_ram_bytes()` failed) ⇒ `true`.
  We only refuse when we AFFIRMATIVELY measure a load won't fit — we never break a working Mac on a
  broken probe. **A guard that fails CLOSED is a worse bug than the OOM it prevents.** Any new app-wide
  guard (a large DB result set, WAV materialize-on-unlock during `unlock_folder`, a whole-transcript
  allocation) copies this: measure best-effort, refuse only on an affirmative over-budget reading,
  fail open on any probe failure.
- **No new crate.** `available_ram_bytes()` parses `vm_stat` (free + inactive + speculative + purgeable
  page classes × page size); `total_ram_gb()` in `commands/model_perf.rs` shells `sysctl -n hw.memsize`. Both use
  `std::process::Command`, NOT a `sysinfo`-style crate — stay on that pattern (no-new-deps).
- **Guards the NEXT load, not eviction.** Because the cache never evicts, the check is purely "is there
  headroom for the next model on top of what's already resident."
- **Tested pure:** `ram_permits_load_ok_when_free_and_refuses_under_pressure`,
  `ram_permits_load_fails_open_on_broken_probe`, `ram_permits_load_small_model_always_ok`.

## Thermal — the co-owned line (never throttle the batch pipeline)

`src-tauri/src/thermal.rs` `ThermalGovernor`/`ThermalLevel` maps `NSProcessInfo.thermalState` to a PURE
back-off policy for the **LIVE loop only**. The module header states the invariant verbatim: *"The
RECORDING and the post-Stop batch pipeline are NEVER touched by this governor — only the best-effort
live loop backs off."* The authoritative transcript is produced at Stop by `pipeline.rs::run_after_stop`
regardless of thermal state. Degrade the ladder (Nominal 3s → Fair 6s → Serious 9s + pause reactions →
Critical suspend captions), hysteresis is degrade-fast/recover-slow (`RECOVER_AFTER_READS`), and the
FFI read degrades to `Nominal` on any doubt (never a crash — it's the typed
`NSProcessInfo::processInfo().thermalState()` getter, not a raw `msg_send`).

**This is the co-review boundary with `model-perf-engineer`.** The thermal tick-policy and the
`mistral.rs` RAM-guard half sit on the app-perf ↔ model-perf seam — neither agent edits the other's core
alone. If you change the tick ladder, the reaction/caption gating, or the RAM headroom factor,
co-review with `model-perf-engineer`; app-perf owns "does it stay responsive," model-perf owns "tokens/sec
+ Metal residency."

## Honest limits (say so — don't green-wash)

- A real **thermal throttle** and a real **1h-meeting Mac-kill** need a real Mac under sustained load +
  `powermetrics`/`footprint`/Activity Monitor — a headless `cargo test` can't reproduce the swap-death.
- `cargo instruments` needs full Xcode; on the CLT-only dev Mac use `samply` and note the gap.
- Binary-size/steady-state wins from `[profile.release]` are measured on a release build; confirm it still
  notarizes (`release-engineer`).
- The perf-budget verdict is `adversarial-verifier`'s. State the numbers you measured and the scenarios
  you could NOT measure headless.
