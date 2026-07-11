# War stories & invariants — the four crash/leak rules + the guard directions

The load-bearing part of this skill: every rule below is a bug that already shipped or aborted the
process. Each has an exact fix SYMBOL and a reproduction. Never regress one to chase speed. Symbols
verified against `reason/mistral.rs`, `reason.rs`, `transcribe/{vad,parakeet,live_asr}.rs`,
`thermal.rs`, `transcribe/model.rs` — confirm before relying.

---

## Rule 1 — Prefix-cache corrupts on the 2nd+ generation → NaN logits

**Fix symbol:** `.with_prefix_cache_n(None)` on the `GgufModelBuilder` in `reason/mistral.rs`
(`MistralReasoner::model`).

**What happened.** mistralrs' default sequence-level prefix caching (`Some(16)` = ON) reuses the
KV-cache **prefix** across requests that share a leading system prompt. Every brain call sends the
same system prompt, so every call hit the shared prefix. On the non-paged Metal GGUF path that
cached prefix goes stale/corrupt after the first few generations, so the 2nd+ call samples from
NaN/Inf logits and errors: *"Invalid sampling probability at index 0: NaN. The model likely
produced NaN/Inf logits."* User-visible symptom: the note-assistant worked the first time and
failed **"za drugim razem"** (the second time).

**Why the fix is free.** Our brain calls are ONE-SHOT completions, not multi-turn chat, so
disabling the prefix cache just re-prefills a short system prompt each call — negligible cost next
to correctness. Every request now gets a FRESH KV cache.

**Never do.** Re-enable `with_prefix_cache_n(Some(n))` to "reuse the cache / speed up prefill".
It is a correctness fix, not a perf choice. Diagnose recurrences via the dev brain log
(`<app-data>/MeetNotes-dev/murmur.log`) for the NaN sampling error.

---

## Rule 2 — REFUSE-don't-evict at the residency cap (mistralrs drop-leaks)

**Fix symbols:** `MODEL_CACHE_CAP: usize = 2` + the process-global `model_cache` in
`reason/mistral.rs`; the caller degrades to the deterministic floor (`StubReasoner`) on `Err`.

**What happened / why.** mistralrs has documented drop-leaks (upstream issues #723/#865): dropping
a loaded engine leaks or aborts. So Murmur NEVER unloads. Loaded engines live in a PROCESS-GLOBAL
cache keyed by canonical GGUF path so the light + heavy engines co-reside and multi-GB weights load
once. The cache is capped at 2; a **3rd distinct model is refused** (`Err`), and the caller drops
to the floor while the FE can nudge "restart to switch models". Until a real-Mac spike (Spike B)
proves clean drops, eviction stays forbidden.

**The RAM guard layered on top (the OOM backstop).** Before the NEXT load, `MistralReasoner::model`
gates on measured free RAM. All PURE + injectable so the decision is headless-testable:

- `ram_permits_load(free_bytes: Option<u64>, model_disk_bytes: u64) -> bool`:
  - a small model (`< MODEL_RAM_GUARD_MIN_DISK_BYTES` ≈ 1.5 GB) ⇒ always `true` (never the OOM
    driver, never risk a false refuse — **fail-open by size**);
  - `free_bytes == None` (probe failed) ⇒ always `true` — **FAIL OPEN**; never break a working
    machine on a broken probe;
  - otherwise ⇒ `free >= model_disk_bytes × MODEL_RAM_HEADROOM_NUM / MODEL_RAM_HEADROOM_DEN`
    (× 3/2, the conservative KV+activations headroom factor).
- `available_ram_bytes() -> Option<u64>` — best-effort AVAILABLE RAM via `vm_stat`, summing the
  reclaimable page classes (free + inactive + speculative + purgeable, NOT wired/active/compressed),
  returning `None` on any parse/exec failure so the caller fails open.

**Tests to preserve** (`reason/mistral.rs`): `ram_permits_load_ok_when_free_and_refuses_under_pressure`,
the broken-probe-fails-open case, the small-model-never-guarded case.

**Never do.** Add an eviction/`drop` path; make the RAM guard refuse on a `None` probe; guard a
tiny model. The guard exists to catch "load a multi-GB model when free RAM is nearly exhausted",
NOT to reject a normal load.

---

## Rule 3 — Only ONE ggml Metal context; the live path stays OFF Metal

**Fix symbols:** `VadSegmenter::load` sets `params.set_use_gpu(false)` (`transcribe/vad.rs`);
`ParakeetAsr::load` uses the CPU provider with `PARAKEET_NUM_THREADS = 4` and NO coreml/metal
provider (`transcribe/parakeet.rs`).

**What happened.** Bringing up a SECOND ggml/whisper Metal context alongside the main whisper Metal
context makes ggml's backend scheduler `ggml_abort` during graph init
(`whisper_vad_init_with_params` → `ggml_backend_sched_alloc_graph`) — a hard C abort Rust can't
catch, crashing the process.

**The policy.** Metal is a scarce single resource owned by whisper-BATCH + the brain. Everything on
the LIVE path is CPU/off-Metal:
- **VAD** (Silero, ~885 kB) — CPU; Metal buys nothing and risks the abort.
- **Parakeet live-ASR** (`parakeet.rs`, NVIDIA parakeet-tdt-0.6b-v3 int8 via sherpa-onnx
  nemo_transducer) — CPU x4 DELIBERATELY off Metal so the shared GPU stays free for the brain. It's
  a safe Rust wrapper over a static onnxruntime (no `msg_send`, no throwing FFI); a null recognizer
  is a plain `Err`. `PARAKEET_MIN_RAM_BYTES = 8 GB` refuses it on affirmatively-below-8-GB machines
  (fail-open probe, parity with the reasoner's whisper-large refuse).
- The `LiveAsr` seam (`transcribe/live_asr.rs`: trait `LiveAsr`, `should_use_parakeet`,
  `build_live_asr`, `ParakeetModelPaths`) lets the live loop swap engines (whisper-Fast vs parakeet)
  engine-agnostically — a one-time info/warn log on fallback, NEVER a hard failure.

**Never do.** Add a second GPU/Metal context on the live path; set a `provider = "coreml"/"metal"`
on parakeet; GPU the VAD.

---

## Rule 4 — No unguarded Obj-C selector across the FFI boundary

**Fix symbol:** `thermal.rs::read_thermal_level` — the typed `objc2_foundation::NSProcessInfo::processInfo().thermalState()` getter wrapped in `std::panic::catch_unwind`, degrading to `ThermalLevel::Nominal` on ANY doubt.

**What happened (the class).** A prior screen-share probe sent `msg_send![screen, isCaptured]`;
`NSScreen.isCaptured` is an iOS-only selector, so it raised an unrecognized-selector `NSException`
that unwound across FFI and ABORTED the process at launch ("Rust cannot catch foreign exceptions").

**The policy for any model FFI probe** (thermal state today; a future Core ML / Metal
introspection): use a TYPED CoreFoundation / `NSProcessInfo` getter that's guaranteed present, wrap
it in `catch_unwind`, and degrade to a safe default — never send a raw selector you haven't proven
the receiver implements. `read_thermal_level` maps `thermalState()` to `ThermalLevel`, and any
unknown/future `NSProcessInfoThermalState` value degrades to `Nominal` (no wrong-side back-off). The
QoS tag (`set_utility_qos`, `pthread_set_qos_class_self_np`, `QOS_CLASS_UTILITY`) is a plain C
function that returns an error code — no exception surface.

**Never do.** An unguarded `msg_send!` on the model path; a wrong-side thermal default.

---

## The thermal governor (fail-safe by construction, `thermal.rs`)

The governor is PURE (no FFI inside): the caller feeds one `ThermalLevel` observation per tick
(from `read_thermal_level`) and reads the policy out.

- `ThermalGovernor::observe(read)` — worse-or-equal thermal ⇒ adopt immediately (**degrade fast**);
  recovery to a lighter level needs `RECOVERY_HYSTERESIS` consecutive better reads (**recover slow**,
  no flapping).
- `effective_tick() -> Duration` — the live-loop tick at the current level (Nominal ~3 s →
  Fair ~6 s → Serious+ ~9 s).
- `reactions_paused()` — whisper "reactions" pause under pressure.
- `captions_suspended()` — captions NEVER hard-suspend on thermal back-off (a user-facing
  "listening" state must not freeze); they only slow via the tick.
- `TurnDefer::should_skip(user_turn_in_progress, live_is_local_gguf)` — defer a heavy local-GGUF
  turn while the user is mid-utterance.

The RECORDING and the post-Stop batch pipeline are NEVER touched by the governor — only the live
caption/reactions loop backs off. Preserve that scope.

---

## The two guard directions (memorize — they're the difference between a fix and a brick)

- **RAM / load guards FAIL OPEN.** A broken probe or a tiny model always loads. Refuse ONLY on an
  affirmative measurement that a load won't fit. A guard that false-refuses a healthy machine is a
  worse bug than the OOM it prevents. (`ram_permits_load`, `PARAKEET_MIN_RAM_BYTES`.)
- **Default / model-size decisions FAIL SMALL.** Degrade to the `small` whisper model / the light
  brain / the `StubReasoner` floor — never OOM. `default_model_size` (`transcribe/model.rs`) only
  upgrades a FRESH install to turbo-q8_0 on ≥ 12 GB (`TURBO_DEFAULT_MIN_RAM_BYTES`); an existing
  install never auto-upgrades off `small`.

Any new guard you add MUST keep its correct direction. Add a PURE unit test that pins it (the
existing `ram_permits_load_*` and `residency_fits` tests are the pattern).
