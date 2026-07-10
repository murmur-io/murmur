# Quantization & KV-cache — the bandwidth ladder

Deep material for the quant/KV lever. All symbols verified against the tree — cite by symbol,
confirm before relying (`reason/mistral.rs`, `reason.rs`, `transcribe/model.rs` all drift).

## Why decode is bandwidth-bound (the governing equation)

On a unified-memory Apple-Silicon Mac, autoregressive **decode** reads the entire resident weight
set once per generated token. Per-token latency is dominated by:

```
t_token ≈ (bytes of weights touched per token) / (memory bandwidth)
tok/s   ≈ memory_bandwidth / resident_weight_bytes
```

Compute (the matmuls) is not the ceiling for a small local model on Metal — the bus is. This is
why:

- **Quantization is a fit/bandwidth lever.** Q4_K_M moves ~half the bytes of Q8_0 per token → up
  to ~2× the decode rate AND leaves more RAM headroom — the trade is quality.
- **Prefill (the prompt) is compute-bound**, decode (generation) is bandwidth-bound. A long
  transcript prompt is a prefill cost (charge it to the KV cache + activation headroom), separate
  from tok/s.
- **Co-residency is a bandwidth tax, not just a RAM tax.** Two models resident during a live call
  contend for the same bus → each runs slower than it would alone. That is the real reason for the
  cap-2 budget, not just the RAM ceiling.

## The GGUF brain quant ladder (mistralrs)

The registry (`BrainModel` in `reason.rs`) ships **Q4_K_M** GGUFs today — e.g. `qwen3-1.7b`
(light, `Qwen_Qwen3-1.7B-Q4_K_M.gguf`, `arch: "qwen3"`, `min_ram_gb: 4`), plus Bielik-v3 and the
large multilingual option. The architecture parser only accepts `llama` / `qwen2` / `qwen3` (NOT
`qwen35`/`qwen3vl`) and every id is Apache-2.0/Bielik-v3 licensed (the Qwen *Research* License is
banned — see `RETIRED_BRAIN_MODELS`).

Quant deltas (rules of thumb — MEASURE on a Mac, never quote as fact):

| Quant | ~size vs Q8 | decode speed | quality | when |
| --- | --- | --- | --- | --- |
| Q4_K_M | ~50% | fastest | good (the knee) | the shipped default |
| Q5_K_M | ~62% | medium | better | if Q4 loses Polish nuance and RAM allows |
| Q8_0 | 100% | slowest of the three | ~lossless | rarely worth it for a chat-class local brain |

To add a quant option: add a `BrainModel` row (id, filename, url, `min_ram_gb`, `arch`), confirm
`arch` is one the mistralrs parser accepts, keep the license invariant, and let
`default_model_for_class` / `class_model_id` continue to select on class + config. The load path
(`MistralReasoner::model` in `reason/mistral.rs`) is quant-agnostic — it just loads the GGUF.

## The KV cache & the prefix-cache correctness trap

- **KV cache** grows with context length; a long transcript prompt inflates peak resident RAM well
  beyond the on-disk weight size. `MODEL_RAM_HEADROOM_NUM/DEN = 3/2` in `reason/mistral.rs` is the
  conservative ×1.5 factor applied to the GGUF disk size to estimate true peak footprint (weights
  stay quantized in RAM ≈ disk size; KV + Metal buffers + activations add on top).
- **Prefix cache (cross-request) is DISABLED — and it's correctness, not perf.**
  `.with_prefix_cache_n(None)` (default is `Some(16)` = ON). mistralrs reuses the KV **prefix**
  across requests that share a leading system prompt — which every brain call does — and on the
  non-paged Metal GGUF path that cached prefix corrupts after the first few generations, so the
  2nd+ call samples NaN/Inf logits. Our brain calls are one-shot completions, so re-prefilling the
  short system prompt each call is negligible next to correctness. **Never re-enable it to "reuse
  the cache."** (Full reproduction in `war-stories-and-invariants.md`.)

## Co-residency RAM math (`reason.rs`)

- `combined_residency_gb(models: &[&BrainModel], call_overhead_gb: u64) -> u64` — the honest
  estimate for a SET of resident models: it sums a coarse per-model footprint PLUS a KV budget,
  because each `BrainModel.min_ram_gb` is **per-model-ALONE and lies** for co-residency during a
  live call.
- `CALL_OVERHEAD_GB` — a fixed budget for the OS + a Zoom/Meet call running alongside the models.
- `residency_fits(models, call_overhead_gb, total_ram)` — `true` iff
  `combined_residency_gb(...) <= total`. Use THIS (not per-model floors) when deciding whether the
  light+heavy pair can co-reside during a live meeting.
- The process-global weight cache (`MODEL_CACHE_CAP = 2` in `reason/mistral.rs`) is the hard cap:
  the light + heavy engines can co-reside, a 3rd distinct model is **refused** (`Err`), never
  evicted (mistralrs drop-leaks — see the war stories).

## The whisper quant default (`transcribe/model.rs`)

Whisper's fresh-install default is the turbo quant, chosen for exactly the bandwidth reason above:

- `TURBO_DEFAULT_SIZE = "large-v3-turbo-q8_0"` (`TURBO_DEFAULT_FILE = "ggml-large-v3-turbo-q8_0.bin"`)
  — MEASURED to run an Accurate batch in the SAME wall-clock as `small` with near-perfect Polish,
  at ~875 MB download.
- `default_model_size` picks it ONLY when (1) already downloaded, or (2) a fresh install on a
  machine with total RAM ≥ `TURBO_DEFAULT_MIN_RAM_BYTES` (12 GB). Otherwise → `small`. An existing
  install with any whisper model on disk **never** auto-upgrades off `small` (the conservative
  fail-small rule). The available quant tiers (`small-q8_0`, `medium-q8_0`, `large-v3-turbo`,
  `large-v3-turbo-q8_0`, `large-v3-q5_0`, plain `small`…) live in the same registry.

## Unexploited quant/KV levers (SPIKES — none wired today)

Confirmed ABSENT in the tree (no `ISQ`/`AFQ`/`fp8`/`speculative`-decode inference code; the
`parakeet.rs` comment explicitly says NO coreml/metal provider). Propose each as a Mac spike with
the risk named:

- **FP8 KV-cache quant** — halve KV footprint → longer context / more headroom on a fixed RAM
  budget. Risk: mistralrs 0.8.1 Metal support for FP8 KV; quality on long transcript prompts.
- **ISQ / AFQ in-situ quant** — load a higher-precision source and quantize at load, trading load
  time for a smaller resident set. Risk: load-time cost; Metal path correctness.
- **Qwen3-0.6B speculative-decode draft** — a tiny draft model proposing tokens the light brain
  verifies, ~1.5–2× decode. Risk: draft/target divergence hurting quality; the draft is a SECOND
  resident model that must fit under `MODEL_CACHE_CAP` / `residency_fits`.

Before proposing: confirm mistralrs 0.8.1 actually exposes the knob (WebFetch the crate docs /
source — do not assume the API), and require a real-Mac A/B (tok/s + quality) before any merge.
