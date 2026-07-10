---
name: app-perf-audit
description: Murmur app-wide performance map + a runnable measurement recipe across the Rust->IPC->Angular seam — FE change-detection anti-patterns, Tauri event-vs-Channel + payload windowing, OOM/RAM-pressure guards, deferred startup, the missing [profile.release], and the profiling toolchain. Use when the user reports jank/lag/heat/OOM, before a release, when adding a feature that touches a hot path, or on any 'why is X slow'. Not for model tokens/sec or quantization (that is ondevice-model-perf).
---

# /app-perf-audit — Murmur app-wide performance

Murmur is a Tauri 2.11 Rust core hosting an Angular 22 **zoneless** webview, macOS-first and
local-first. "App performance" here = **does the app stay responsive and not get jettisoned by the
OS** — startup latency, the Rust→IPC→Angular data seam, memory/OOM pressure, and the zoneless
change-detection budget. This is NOT model tokens/sec, quantization, or decode strategy — that is the
`model-perf-engineer` / `ondevice-model-perf` surface. The two overlap ONLY on `thermal.rs`
tick-policy + the `mistral.rs` RAM-guard half (co-reviewed, never edited alone).

**Pairs with the `app-perf-engineer` agent** — dispatch it (or follow this skill yourself); the
perf-budget verdict is `adversarial-verifier`'s, not the author's.

**Measure before you touch.** A perf edit with no before/after number is a guess. Attach a profiler,
get the number, fix minimally in-style, re-measure. Verify FE perf on **WebKit, not Chromium**
(the T4 CSP trap — a green `ng serve` proves nothing about the packaged build).

Deep, code-grounded material lives in the references — load the one that matches the layer:

- **references/fe-cd-checklist.md** — zoneless change-detection anti-patterns: O(n) template
  method/getter → `computed`; stale-result guards; un-windowed `@for` + `RENDER_CAP`; verify on
  WebKit; `afterNextRender` not `setTimeout`.
- **references/ipc-payload-patterns.md** — `app.emit(large/high-freq)` + per-tick polling →
  `tauri::ipc::Channel<T>`; paginate whole-transcript reads; the DTO-shape-for-lock trap (never a
  plaintext path for a locked meeting).
- **references/rust-profiling-recipe.md** — `samply`/`cargo-instruments` + `tokio-console` + `tracing`
  spans; the `[profile.release]` block; the OOM anatomy (residency + un-chunked IPC + DOM node count +
  missing RAM guard) and where each guard lives.

## FE audit (zoneless change detection)

The trap: under zoneless CD (`provideZonelessChangeDetection` in `src/app/app.config.ts`), a
**method/getter called from a template re-runs on every CD pass** — for a per-tick binding over a long
list that is an eval-storm. The shipped fix is
`src/app/features/detail/audio-panel/audio-panel.component.ts` `activeSegKeys` (a `computed<Set>`
scanned once per tick, `.has()` O(1) in the template — it replaced an `isActiveSegment()` method that
Angular re-ran O(n) per fragment per pass) + the `RENDER_CAP = 80` window (`visibleTurns`/
`renderedTurns`, no `@angular/cdk`). Every effect-orchestrated IPC fetch needs a stale-result guard
(`entity-detail.component.ts` `_load`). → **references/fe-cd-checklist.md**.

## IPC audit (event vs Channel, payload size)

`app.emit(EVENT, payload)` JSON-serializes the payload and evals it into the webview on EVERY emit.
There is **no `Channel<T>` in the tree today** — every stream (`EVENT_LIVE_CAPTION`, `EVENT_STATUS`,
whisper cards, proactive hints in `transcribe/live.rs` / `pipeline.rs`) is an emit. For a
high-frequency or large stream that is the wrong tool: reach for `tauri::ipc::Channel<T>`. For a
whole-transcript read, paginate/window instead of shipping the whole array. Any read refactor keeps its
lock gate (`meeting_is_unlocked` / `visibility_clause`), and a masked DTO never hands the FE an on-disk
audio path (the `convertFileSrc`/`asset:` leak — `detail.component.ts` `audioSrc`).
→ **references/ipc-payload-patterns.md**.

## Rust profiling recipe

`samply record` (or `cargo instruments -t "Time Profiler"`) around the scenario for on-CPU cost —
BUT async/IO/DB-lock/Metal-wait is invisible to a CPU sampler, so pair it with **`tokio-console`** (task
stalls) + **`tracing` spans** (`target:` + non-PII counts/durations). Loop with
`( cd src-tauri && cargo test --lib )` — **never `cargo clippy --all-targets`** (thrashes the
openssl/sqlcipher profile). `[profile.release]` is **missing** from `src-tauri/Cargo.toml` today — add
`lto`/`codegen-units=1`/`strip` (config, not a dependency). → **references/rust-profiling-recipe.md**.

## Memory audit (OOM / RAM pressure)

Opening a 1h meeting has killed the Mac. The cause is **cumulative**, not one allocation: never-evict
model residency (`MODEL_CACHE_CAP = 2`, REFUSE-don't-evict in `reason/mistral.rs`) + an un-chunked
transcript into a local model + the DOM node count of an un-windowed `@for` + a **missing RAM guard** on
the allocation — tipped by on-open work. The RAM guard `ram_permits_load` **FAILS OPEN** on a broken
`vm_stat` probe (never break a working Mac); the transcript OOM guard is UNIFORM DECIMATION in
`summarize/timeline.rs`. Extend the same swap-death-backstop discipline app-wide (DB reads, WAV
materialize-on-unlock), always fail-open. → **references/rust-profiling-recipe.md** (OOM anatomy).

## Guardrails (binding — do not trade correctness for speed)

- **Startup stays non-blocking + non-panic.** No blocking work in `.setup()` (`lib.rs`); heavy work →
  `spawn_blocking` or a detached loop whose first tick is a full interval after launch. An init failure
  is a graceful dialog + `std::process::exit(1)`, NEVER `unwrap`/`expect`.
- **Never throttle recording or the post-Stop batch pipeline.** The thermal governor (`thermal.rs`)
  touches only the best-effort LIVE loop. Co-review `thermal.rs` + the `mistral.rs` RAM half with
  `model-perf-engineer`.
- **The RAM guard FAILS OPEN and is app-wide.** A fail-closed guard is worse than the OOM.
- **Keep every read gated.** A windowed/paginated/cached read still routes through
  `meeting_is_unlocked`/`visibility_clause`; a masked DTO never leaks an on-disk path. Speeding up a
  leak is still a leak. Lock-path refactor → `lock-security-reviewer`.
- **No new deps, metrics local, no PII.** `RENDER_CAP` before `@angular/cdk`; `Channel<T>` before a new
  npm stream lib; IDs/stages/counts/durations only in a metric — never note/transcript text.
- **FE perf is a WebKit claim** (T4); `cargo test --lib` not `clippy --all-targets`; the perf-budget
  verdict is `adversarial-verifier`'s, not the author's.
