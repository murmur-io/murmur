# Measurement harnesses — compile-proven vs signed-Mac-proven

The honesty bar for the model layer. Read this before you quote ANY speed / watt / WER / tok-s
number. Harness symbols verified against `transcribe/whisper.rs`, `transcribe/parakeet_spike.rs`,
`embed/candle_bert.rs`, `summarize/ner_deberta.rs`, `reason/mistral.rs` — cite by symbol.

## The hard line: `cargo test --lib` runs NO forward pass

The heavy ML tree (mistralrs/candle/whisper) is ALWAYS compiled, so `cargo test --lib` links the
real impls — but every real inference test is `#[ignore]`d and skip-soft (missing env → print +
return, never panic). The module headers say it explicitly:

- `reason/mistral.rs`: *"`cargo test --lib` NEVER runs a forward pass here. Treat a green build as
  proof the impl typechecks/links against mistralrs 0.8.1 — NOT as proof inference works."* Same
  for real correctness, the token cap / `enable_thinking`, Polish quality, load time, tok/s,
  memory, and the cap-2 co-residency / drop behavior.
- `embed/candle_bert.rs` and `summarize/ner_deberta.rs`: the smoke tests are `#[ignore]`d; Metal
  correctness + the Metal-vs-CPU fallback (`pick_device`: `Device::new_metal(0)` first, `Device::Cpu`
  fallback) only verify on a Mac.

So: **a green `cargo test --lib` proves the pure DECISION boundaries** — `ram_permits_load`,
`residency_fits`/`combined_residency_gb`, `audio_ctx_for`, `default_model_size`, the
`ThermalGovernor` state machine, `grammar_constraint_applies` — and NOTHING about a number. State
the test names + pass counts; then move to the Mac list.

## Watts & thermals — `powermetrics` alongside the live duty cycle

The instrument: `live_duty_cycle_sim_from_env` (`#[ignore]`, `transcribe/whisper.rs`) simulates the
LIVE loop's real wall-clock duty cycle — it sleeps the tick remainder so `powermetrics` sampling
alongside sees the true duty cycle, and prints a `DUTY_RESULT` line
(`model=… gate=… audio_ctx=… ticks=… decodes=… decode_ms_total=… wall_s=… duty_pct=…`).

```bash
# On a signed/dev build with the model + Silero VAD in the app models dir:
MURMUR_DUTY_WAV=~/asr/meeting-16k.wav \
MURMUR_DUTY_MODEL=~/Library/Application\ Support/MeetNotes/models/ggml-small.bin \
MURMUR_DUTY_MINUTES=5 MURMUR_DUTY_GATE=1 MURMUR_DUTY_AUDIO_CTX=832 \
  cargo test --lib live_duty_cycle_sim_from_env -- --ignored --nocapture

# In another terminal, sample package/GPU/CPU power + thermal state:
sudo powermetrics --samplers cpu_power,gpu_power,thermal -i 1000
```

Read: `duty_pct` (fraction of wall-clock the decoder is busy) and package/GPU watts. Also confirm
`ThermalGovernor::effective_tick` actually stretches under real thermal pressure. NOTE: the current
heat fix is MODELED (VAD tick-gate + flash-attn + audio_ctx=832 + QoS/thermal governor); watts are
**pending a real `powermetrics` pass** — say "watts pending", never quote a modeled watt figure as
measured.

## WER / Polish quality — the fixed PL/EN WAV + the A/B harness

Record a fixed ~10-min PL/EN 16 kHz mono WAV ONCE and reuse it for every ASR claim (a fixed corpus
is the only way an A/B is comparable).

- **Whisper A/B** — `asr_ab_harness_from_env` (`#[ignore]`, `transcribe/whisper.rs`): decode the
  SAME WAV through TWO models at BOTH profiles, printing wall-clock per leg and writing each
  transcript to a temp file for diffing. This is the instrument behind every flash-attn /
  audio_ctx=832 / quant / live-pin claim.
  ```bash
  MURMUR_ASR_AB_WAV=~/asr/meeting-16k.wav \
  MURMUR_ASR_AB_MODEL_A=~/Library/Application\ Support/MeetNotes/models/ggml-small.bin \
  MURMUR_ASR_AB_MODEL_B=.../ggml-large-v3-turbo-q8_0.bin \
  MURMUR_ASR_AB_LANG=pl \
    cargo test --lib asr_ab_harness_from_env -- --ignored --nocapture
  ```
  WER method: transcribe the WAV, diff against a human reference transcript (word-error-rate).
  The turbo-q8_0-vs-small MEASURED result ("same wall-clock, near-perfect Polish") came from
  exactly this harness — reproduce it, don't cite it blind.

- **Parakeet live-ASR** — `parakeet_spike_from_env` (`#[ignore]`, `transcribe/parakeet_spike.rs`):
  decodes `MURMUR_ASR_AB_WAV` with the int8 transducer from `MURMUR_PARAKEET_DIR`
  (encoder/decoder/joiner `.int8.onnx` + `tokens.txt`) on CPU x4, and reports RTF (audio_secs /
  wall) + the text (written to a temp file).
  ```bash
  MURMUR_PARAKEET_DIR=~/asr/parakeet-tdt-0.6b-v3-int8 \
  MURMUR_ASR_AB_WAV=~/asr/meeting-16k.wav \
    cargo test --lib parakeet_spike_from_env -- --ignored --nocapture
  ```
  The prior measurement: parakeet int8 ≈ 13× real-time on 4 CPU threads OFF Metal — the live-ASR
  candidate. RTF < 1 means real-time-capable.

## tok/s + load time + residency + the refuse firing (brain)

No `#[ignore]` harness prints these — the brain path only reports them on a real forward pass
(mistral.rs is explicit). On a signed/dev Mac with a GGUF present:

- **tok/s** — time a real `reason`/`structured` generation, tokens / wall-clock.
- **load time** — time the first (lazy) `MistralReasoner::model` call (weights load once, cached).
- **residency** — Activity Monitor / `footprint` on the `Murmur` process; confirm
  `combined_residency_gb` ≈ observed when light+heavy co-reside.
- **the refuse** — load a 3rd distinct model and confirm it returns `Err` (not an evict/crash), and
  that the RAM guard refuses a big model on a memory-starved machine (and fails open when `vm_stat`
  is unavailable). Diagnose via `<app-data>/MeetNotes-dev/murmur.log`.

## Embedder / NER / diarize Metal-vs-CPU

`#[ignore]` candle smokes need the model dir on disk (`embed/candle_bert.rs`,
`summarize/ner_deberta.rs`). On a Mac: confirm `pick_device` picks Metal, the forward pass produces
sane output (e5 mean-pool+L2 vectors; DeBERTa BIO spans), and the CPU fallback path works when Metal
init fails. Throughput (texts/sec) is the runtime-cost number — the retrieval QUALITY (recall/nDCG)
is out of scope here (that's the retrieval surface).

## afm / Apple Foundation / ANE — DEFERRED

`reason/afm.rs` (`AfmReasoner`) is a Swift-sidecar seam that is NOT built on this CLT-only machine
(no macOS-26 SDK); with the sidecar absent it degrades to `StubReasoner`. Its on-device / ANE
performance can ONLY be measured on a signed macOS-26 Mac with the native sidecar written and
pinned to `SystemLanguageModel.default`. Anything ANE/afm is a "needs a macOS-26 Mac" line, never a
measured claim.

## The reporting rule

Split every result into **modeled** (asserted from code/math) vs **measured** (observed on a Mac,
with the command that produced it). Never present a modeled number as measured. The PASS/FAIL
verdict on a perf/quality/thermal number belongs to `adversarial-verifier` on a signed Mac — not to
the implementer.
