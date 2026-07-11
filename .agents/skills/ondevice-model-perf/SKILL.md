---
name: ondevice-model-perf
description: Deep on-device model-performance context for Murmur on Apple Silicon Metal — the bandwidth-bound mental model, the quant/KV-cache ladder, whisper flash-attn/audio_ctx/profile discipline, the four crash/leak war stories, residency/RAM math, and the measurement bar. Use whenever a task is about MODEL-level performance / thermals / RAM (whisper.cpp / mistralrs / candle / parakeet) — tuning a quant or KV-cache, the Mac running hot, note-gen/ASR speed, the brain OOMing, the thermal governor, or a speculative-decode / Core ML ANE spike. NOT app IPC/UI/startup (use app-perf-audit) and NOT retrieval quality (use retrieval-memory-brain — this covers only the embedder/reranker/NER runtime cost).
---

# /ondevice-model-perf — Murmur's on-device inference knowledge base

Murmur runs four model families **in-process on Apple-Silicon Metal**: whisper.cpp ASR
(`whisper-rs` 0.16), the mistralrs 0.8.1 GGUF brain, candle e5/DeBERTa (embedder + NER), and
the parakeet int8 live-ASR engine (sherpa-onnx, off Metal). This skill is the dense,
code-grounded context for making them **fast, good, and cool** without regressing the crash/leak
invariants they already paid for. It does NOT cover app-level latency (IPC/UI/startup/DB — that's
the app-perf surface) or retrieval QUALITY (recall/nDCG/fusion — that's the retrieval surface);
here the embedder/reranker/NER matter only as **runtime cost**.

Binding rules live in `.codex/rules/rust-tauri.md` (§7 crash-safe FFI, §8 no-PII-logs, §9
`cargo test --lib`-not-`clippy --all-targets`) — this skill does not restate them; it points at
the model-layer specifics. **Trust code, not this doc:** cite by SYMBOL and confirm it still
exists before relying on it.

**Pairs with the `model-perf-engineer` agent** — dispatch it (or follow this skill yourself) for a
model-layer tune/harden/debug. It self-checks but does NOT self-certify perf numbers; watts / WER /
tok-s verdicts go to `adversarial-verifier` on a signed Mac.

## The map (load a reference on demand — don't inline it)

| When the task is about… | Load |
| --- | --- |
| quant choice (Q4_K_M/Q5/Q8), KV-cache/prefix cache, FP8 KV quant, ISQ/AFQ, co-residency math | `references/quant-and-kv-cache.md` |
| whisper Fast vs Accurate, flash-attn, `audio_ctx`, beam/temperature/anti-hallucination gates, VAD | `references/whisper-profiles.md` |
| a crash/leak/abort, prefix-cache NaN, refuse-don't-evict, second-Metal-context, FFI safety | `references/war-stories-and-invariants.md` |
| running a real measurement (watts / WER / tok-s), the `#[ignore]` harnesses, the fixed WAV | `references/measurement-harnesses.md` |

## Apple-Silicon mental model (the one thing to internalize)

Decode on a unified-memory Mac is **memory-bandwidth-bound**, not compute-bound: per-token
latency ≈ (resident weight bytes touched) / (memory bandwidth). Consequences that drive every
lever here:

- **Quantization is a bandwidth/fit lever, not a FLOPs lever.** A smaller quant moves fewer bytes
  per token → faster decode AND fits more headroom — at some quality cost.
- **Co-residency halves effective bandwidth.** Two models resident during a live call contend for
  the same memory bus; this is why the brain cache is capped at 2 and why residency is charged with
  a KV/call overhead the per-model floor lies about (`combined_residency_gb`, `CALL_OVERHEAD_GB` in
  `reason.rs`).
- **Metal is a scarce single resource.** Only ONE ggml Metal context is safe; the live path
  (VAD, parakeet) is deliberately CPU/off-Metal so the GPU stays free for whisper-batch + the brain.

## The quant + KV-cache ladder (summary — detail in the reference)

- Brain GGUFs ship **Q4_K_M** today (`reason.rs` registry: `qwen3-1.7b` light, Bielik, large).
  Q4_K_M ≈ best size/quality knee; Q5/Q8 buy quality at more bandwidth+RAM.
- Whisper ships **`large-v3-turbo-q8_0`** as the fresh-install default (`TURBO_DEFAULT_SIZE`,
  `transcribe/model.rs`): MEASURED same wall-clock as `small` with near-perfect Polish, ~875 MB —
  but only on ≥ 12 GB machines (`TURBO_DEFAULT_MIN_RAM_BYTES`); low-RAM installs **fail small** to
  `small`.
- `with_prefix_cache_n(None)` (`reason/mistral.rs`) is DISABLED cross-request KV prefix caching —
  it is a **correctness** fix (NaN logits), not a perf choice; never re-enable to "reuse the cache".
- Unexploited: FP8 KV-cache quant, ISQ/AFQ in-situ quant, a Qwen3-0.6B speculative-decode draft.
  All are SPIKES (see the reference), none wired today.

## Whisper levers by profile (summary — detail in the reference)

- `flash_attn(true)` — context-level (`whisper.rs`), applies to both profiles (whisper-rs 0.16
  defaults it stale-false; whisper.cpp 1.8.3 wants true).
- `audio_ctx = 832` (`LIVE_AUDIO_CTX`, `audio_ctx_for`) — **Fast/live ONLY**; Accurate keeps full
  context.
- Beam-5 + temperature fallback + entropy/logprob/no-speech gates (`build_params`, `BATCH_*`) —
  **Accurate/batch ONLY, and INVIOLATE**: they are anti-hallucination CORRECTNESS, never a speed
  knob to move onto the live path.

## The four hard rules (never regress — reproductions in the reference)

1. **Prefix-cache off** — `with_prefix_cache_n(None)` in `reason/mistral.rs` (2nd-call NaN logits).
2. **Refuse-don't-evict** — `MODEL_CACHE_CAP = 2`, a 3rd model is `Err` → floor (mistralrs drop-leaks).
3. **No second ggml Metal context** — VAD `set_use_gpu(false)`, parakeet CPU x4, else `ggml_abort`.
4. **No unguarded Obj-C selector across FFI** — typed CF/`NSProcessInfo` getter + `catch_unwind`
   (`thermal.rs::read_thermal_level`); an `NSException` aborts the process.

## Residency & RAM math (summary — detail in the war-stories reference)

- RAM guard is PURE + injectable and **FAILS OPEN**: `ram_permits_load(free, disk)` in
  `reason/mistral.rs` — small model (< ~1.5 GB) always loads; broken probe (`None`) always loads;
  refuse ONLY when `free < disk × 3/2` is affirmatively measured. `available_ram_bytes()` sums
  reclaimable `vm_stat` pages.
- Co-residency budget: `combined_residency_gb(&[light, heavy], CALL_OVERHEAD_GB)` +
  `residency_fits` in `reason.rs` — per-model `min_ram_gb` is per-model-ALONE and lies for a live
  call; the combined guard adds a KV + OS + Zoom/Meet overhead.
- Whisper defaults **fail small**: `default_model_size` picks turbo only when already downloaded or
  RAM ≥ 12 GB; an existing install never auto-upgrades off `small`. Parakeet refuses below 8 GB
  (`PARAKEET_MIN_RAM_BYTES`, fail-open probe).

## The measurement bar (summary — steps in the reference)

`cargo test --lib` runs **NO forward pass** — it proves the pure decision boundaries only.
Everything below needs a **signed/dev build on a real Mac** with the model present:

- **watts** → `sudo powermetrics --samplers cpu_power,gpu_power,thermal -i 1000` alongside the app
  (or the live-duty-cycle `#[ignore]` harness in `whisper.rs`).
- **WER / Polish** → `asr_ab_harness_from_env` (`MURMUR_ASR_AB_WAV` + `MURMUR_ASR_AB_MODEL_A/B`)
  and `parakeet_spike_from_env` over a FIXED PL/EN 16 kHz mono WAV.
- **tok/s + load + residency + the refuse firing** → a real brain forward pass; the embedder/NER
  Metal-vs-CPU smokes are `#[ignore]`d and need the model dir.

Modeled ≠ measured. Present a number only if you observed it on a Mac; otherwise say "modeled /
pending a Mac pass" and hand the verdict to `adversarial-verifier`.

## Unexploited-lever menu (frame each as a spike, with its risk)

| Lever | Expected win | Risk to name |
| --- | --- | --- |
| FP8 KV-cache quant | halves KV footprint → longer context / more headroom | Metal support in mistralrs 0.8.1; quality on long prompts |
| ISQ / AFQ in-situ quant | load a higher-precision GGUF, quantize at load | load-time cost; Metal path correctness |
| Qwen3-0.6B speculative-decode draft | 1.5–2× decode on the light brain | draft/target divergence; a 2nd resident model vs the cap |
| whisper Core ML ANE encoder | encoder off Metal → GPU free, cooler | Core ML build/signing; a NEW FFI surface (§7 guard) |

None is wired today (confirmed absent in the tree). See `references/quant-and-kv-cache.md` and
`references/measurement-harnesses.md` before proposing one.
