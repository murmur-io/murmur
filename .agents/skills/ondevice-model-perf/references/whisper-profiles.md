# Whisper levers by profile — Fast (live) vs Accurate (batch)

Deep material for the whisper.cpp lever. Symbols verified against `transcribe/whisper.rs`,
`transcribe/vad.rs`, `transcribe/model.rs` — cite by symbol, confirm before relying (these files
drift). The one-line rule: **speed levers on Fast, correctness gates on Accurate, never the
reverse.**

## Two profiles, one enum (`TranscribeQuality`)

`transcribe/whisper.rs` defines `TranscribeQuality::Fast` and `::Accurate`. They are chosen by
CALLER, not by a global setting:

- **`Fast`** — the LIVE caption path (`transcribe/live.rs`) and the VOICE-TRIGGER path
  (`audio/listener.rs`). Greedy single-best (`SamplingStrategy::Greedy { best_of: 1 }`), no
  fallback ladder. Runs overlapping windows many times per recording, so LATENCY dominates; a slow
  tick just means less-frequent captions, never a broken recording. `Transcriber::transcribe`
  defaults to `Fast`.
- **`Accurate`** — the BATCH path (`pipeline.rs`, run ONCE after Stop) that produces the
  authoritative, persisted transcript. Beam search + temperature fallback + anti-hallucination
  thresholds + previous-text conditioning. This is the quality authority.

`build_params(quality) -> FullParams` is the single place the two decoder configs are built —
edit it, not the call sites.

## The context-level lever: `flash_attn(true)` (BOTH profiles)

In `Transcriber::new` / the context build, `ctx_params.flash_attn(true)` is set on the
`WhisperContextParameters` before `WhisperContext::new_with_params`. This is **context-level**, so
it applies to both profiles. It matters because **whisper.cpp 1.8.3 defaults `flash_attn = true`
but whisper-rs 0.16's Rust-side `WhisperContextParameters::default()` still carries the OLD
`false`** — so it must be set explicitly or the encoder silently runs the slow attention path.
Flash-attn is a pure speed/thermal win with no quality cost; keep it on.

## The Fast-only lever: `audio_ctx = 832` (`LIVE_AUDIO_CTX`)

`LIVE_AUDIO_CTX: i32 = 832` right-sizes the whisper encoder's `audio_ctx` for the live window.
Rationale (from the code): whisper's encoder default processes the full 30 s mel context; a live
tail is ~14 s, so most of that context is wasted compute. The formula `(len/30)*1500 + 128`
gives `≈ 828 → 832` (rounded up to a multiple of 8); 832 frames cover ~16.6 s per encoder pass —
≥ every Fast caller's window (live ~14 s, voice-trigger shorter). Reference note: this held WER on
base.en at ~3.4× encoder speed.

The dispatch is `audio_ctx_for(quality)`:

```
Fast     => Some(LIVE_AUDIO_CTX)   // set via params.set_audio_ctx(832)
Accurate => None                   // whisper's FULL context — the quality authority
```

**INVARIANT: never set `audio_ctx` on the Accurate/batch path.** The authoritative transcript
must keep the full encoder context. HONEST CAVEAT already in the code: the Polish-neutrality of a
reduced `audio_ctx` is only measured on base.en — a Polish WER A/B on the fixed WAV is the honest
proof if you ever retune 832.

## The Accurate-only gates — INVIOLATE (correctness, not speed)

`build_params(Accurate)` sets, and NONE of these belongs on the Fast path:

- `SamplingStrategy::BeamSearch { beam_size: BATCH_BEAM_SIZE }` — `BATCH_BEAM_SIZE = 5`
  (OpenAI Whisper's reference beam width).
- `set_temperature(BATCH_TEMPERATURE)` + `set_temperature_inc(BATCH_TEMPERATURE_INC)` — the
  temperature-fallback ladder: a segment that trips a gate is retried at the next temperature rung.
- `set_entropy_thold(BATCH_ENTROPY_THOLD)` — the gzip-compression-ratio analog (2.4 reference);
  above it a segment is treated as repetitive/hallucinated and the next rung is tried.
- `set_logprob_thold(BATCH_LOGPROB_THOLD)` — average-logprob gate (below −1.0 = low confidence →
  fallback).
- `set_no_speech_thold(BATCH_NO_SPEECH_THOLD)` — the no-speech gate.
- previous-text conditioning (`no_context` off) — inflection continuity that matters for heavily
  inflected Polish.

These are the **canonical OpenAI-Whisper anti-hallucination loop**. They cost CPU/latency — which
is why Fast omits them (overlapping windows every couple seconds would multiply the per-tick cost
for throwaway captions). Moving any of them onto Fast to "improve live quality" is a correctness
change to a throwaway path at a latency cost you don't want — DON'T. The Fast captions are
explicitly throwaway; the Accurate batch is the truth.

ASR confidence is likewise Accurate-only: `no_speech_probability()` × mean token prob is computed
only on the batch path (the persisted transcript); Fast captions get no confidence.

## VAD — Silero, CPU-only (the second-Metal-context abort)

`transcribe/vad.rs` `VadSegmenter::load` pre-segments the Accurate batch path with whisper.cpp's
native Silero VAD via the standalone `WhisperVadContext`. It sets `params.set_use_gpu(false)`
**on purpose**: the Silero model is tiny (~885 kB, Metal buys nothing) AND running a SECOND ggml
Metal context alongside the main whisper Metal context makes ggml's scheduler `ggml_abort` during
graph init (`whisper_vad_init_with_params` → `ggml_backend_sched_alloc_graph`) — a hard C abort
Rust can't catch. Keep VAD off Metal. (Same family of bug as the live parakeet engine staying CPU
— see `war-stories-and-invariants.md`.)

Note the plumbing trap already documented: `WhisperVadParams` alone does NOT work
(`whisper_full_with_state` has no VAD code and panics if the VAD model path is unset, whisper.cpp
#3402) — the standalone `WhisperVadContext` is the only working path.

## The live tick is thermal-governed (`transcribe/live.rs` × `thermal.rs`)

The live loop's tick period is NOT fixed — `ThermalGovernor::effective_tick()` returns ~3 s at
Nominal and stretches to ~6 s / ~9 s under Fair / Serious+ thermal pressure. Every tick-counted
window in `live.rs` (wake dedup `WAKE_DEDUP_TICKS`, hangovers, backstop budgets) is counted in
TICKS, so under thermal pressure those windows stretch in wall-clock terms WITH the tick — intended
back-off. A slower tick = less-frequent captions, never a broken recording. See
`war-stories-and-invariants.md` for the governor's fail-safe (degrade to `Nominal`, never crash).
