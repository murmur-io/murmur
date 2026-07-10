<!-- Generated 2026-07-09 via /research (3 murmur-researcher angles: whisper.cpp tuning / alternative engines / live-loop + thermal architecture, plus an adversarial fact-check pass over the decisive claims). Companion to 2026-07-09-brain-v2-architecture.md — this is the transcription half of Brain v2. Pricing/versions/benchmarks = point-in-time. Cite-by-symbol; line anchors drift. -->
# Research: Transcription performance — why the Mac runs hot, and the faster/cooler/better plan

**Scope:** the whisper.cpp transcription stack as shipped in 0.8.0 — live captions, batch, wake-word listener — analyzed for speed, energy/thermals, and Polish+English quality; measured against published benchmarks, streaming-ASR best practice, and the current on-device engine landscape; ending in a phased plan that slots into Brain v2 (the live-loop half of this plan and Brain v2's L4 "gated incremental summarizer" are the same rebuild).

---

## TL;DR / Verdict

**The heat is not "whisper is slow" — it is our duty cycle.** The live loop re-decodes the last **14 s of audio every 3 s, unconditionally** (even in silence), with **no VAD gate, no `audio_ctx` right-sizing (every window is padded to a 30 s encoder pass), flash attention silently OFF** (whisper-rs's `false` default overrides upstream whisper.cpp 1.8.3's `true` default), and — unless `brain_live` is on — with the **user-selected model, up to `large-v3`**, which cannot sustain 4.7× realtime and saturates the shared Metal GPU for the entire meeting. There is **zero thermal awareness** anywhere in the app.

Four independently-shippable, zero-new-dependency changes (VAD tick gate, `audio_ctx=832`, unconditional live pin to `small`, `flash_attn(true)` + a thermal governor) combine to a modeled **~6–8× compute cut for small-model users and ~25–50× for large-v3-live users** — past any reasonable heat target before touching the architecture. On top of that:

- **Batch default should become `large-v3-turbo-q8_0` for Polish** — ≈ large-v2 quality (Polish FLEURS ~5.4 vs today's `small` default at 14.7 — ~2.5× fewer errors) at ~1.2 GB and a decoder 8× lighter than large-v3.
- **The engine to watch is NVIDIA parakeet-tdt-0.6b-v3** (Polish confirmed, FLEURS 7.31, CC-BY-4.0): ~10× faster, runs **off Metal**, and is integrable through the **`sherpa-onnx` crate already in our Cargo.toml — zero new dependencies**. It is the live-path candidate; whisper stays the batch quality authority. This is exactly the split MacWhisper/VoiceInk/Whisper Notes converged on.
- **Apple SpeechAnalyzer (macOS 26) is disqualified: no Polish locale** (42 locales, no `pl_*`). Re-evaluate only if Apple adds Polish.
- **Everything above must be gated on measurement**: a `powermetrics` A/B protocol + a fixed Polish/English WAV harness is the first step, not the last — none of the modeled numbers count until measured on a real Mac.

---

## What we already have (grounded, verified in-tree)

| Fact | Where |
|---|---|
| Engine: whisper.cpp via `whisper-rs = "0.16"`, `features=["metal"]` (compile-time Metal); vendored whisper.cpp **1.8.3** | `src-tauri/Cargo.toml`; `whisper-rs-sys` vendored source |
| **Flash attention OFF**: `Transcriber::load` uses `WhisperContextParameters::default()`; whisper-rs 0.16 defaults `flash_attn: false`, overriding upstream 1.8.3's `flash_attn = true` | `transcribe/whisper.rs` (`Transcriber::load`); crate source `whisper_ctx.rs:477,491,584`; vendored `whisper.cpp:3609` |
| Live loop: `TICK = 3000 ms`, `WINDOW_SECS = 14`; snapshot mic tail → resample → `transcribe` **every tick, unconditionally** (only skip: <1 s captured). No VAD, no thermal check, no `audio_ctx`, no `n_threads` on the Fast profile | `transcribe/live.rs` (`TICK`, `WINDOW_SECS`, tick body); `whisper.rs` (`build_params`, Fast arm) |
| Live model = **user-configured (up to large-v3)** unless `brain_live` is on, which pins to `small` — the split already exists conditionally | `commands.rs` (`live_model` selection in record-start) |
| Batch (Accurate): beam 5, temperature ladder, entropy/logprob/no-speech gates, Silero-VAD pre-segmentation (CPU-only — a second ggml Metal context `ggml_abort`s), 120 s max decode windows, dual-stream (mic + system) after Stop | `whisper.rs` (batch consts), `transcribe/vad.rs` (CPU-only rationale), `pipeline.rs` (`transcribe_stream`, `decode_windows`) |
| **Wake-word listener (opt-in, default OFF)**: while armed, opens the mic every ~2.2 s and decodes the window with the **full user-selected model** in a **separately loaded context** — a standby heat/RAM source for users who enable `voice_trigger` | `audio/listener.rs` (`VoiceListener::start`, `WINDOW`), `commands.rs` (`restart_voice_listener`), `settings/config.rs` (`voice_trigger: false`) |
| Metal is **shared**: whisper decode + mistralrs LLM (reactions ~every 21 s + user turns) + candle embedder. The ASR tick never reads the new `user_turn_in_progress` flag (P0.3) — it decodes even mid-user-answer | `live.rs`, `brain_reactions.rs`, `state.rs` |
| Model resolution has **no quant support** (plain sizes only); default `model_size = "small"`; downloads from the `ggerganov/whisper.cpp` HF mirror (which also hosts q5/q8 quants + prebuilt Core ML encoders) | `transcribe/model.rs` (`model_filename`, `model_url`) |
| `sherpa-onnx = "1.13"` (locked **1.13.3**) already in-process for diarization, with static onnxruntime; the same crate exposes `OfflineRecognizer` + `nemo_transducer` (documented with a Parakeet example in the crate source) | `Cargo.toml`, `Cargo.lock`, `transcribe/diarize.rs`; crate source `offline_asr.rs` |
| Swift sidecar pattern proven (sysaudio/audiocap/aeccap/calendar helpers, signed inside-out) — a CoreML sidecar is architecturally acceptable | `tauri.conf.json`, `scripts/macos-sign-notarize.sh` |
| Zero `thermal`/`qos`/`LowPower` references in the tree; `objc2-foundation` + `libc` already in the graph (NSProcessInfo + `pthread_set_qos_class_self_np` reachable, additive) | grep; `Cargo.toml` |

## Findings

### 1. The duty-cycle arithmetic (the heat, modeled)

Loop shape: `sleep(3 s)` → decode work `W`; sustained GPU duty ≈ `W/(3+W)`. With published RTF anchors (M1-era bench issue #89 ratios + 2026 blog-grade Metal ladders; ordering robust, absolutes coarse):

| Config | Sustained GPU duty |
|---|---|
| Today, `small` live | ~15–35% |
| **Today, `large-v3` live** (encode 2–4 s + decode 1–2 s per tick) | **~50–65%, continuous, the whole meeting — the heat complaint** |
| + pin live to `small` | ~15–35% |
| + `audio_ctx = 832` (encoder stops padding 14 s → 30 s) | ~10–27% |
| + VAD tick gate (mic-only speech density ~15–35% in meetings) | **~3–8% average** |
| + thermal tick-stretch 3→6 s under `fair` | ~2–4% |

Modeled combined cut: **~6–8× (small users), ~25–50× (large-v3-live users)**. All modeled — the powermetrics protocol below converts it to fact. The batch pass is a bounded burst (VAD-segmented, runs once) — not the primary heat source. The opt-in wake listener adds standby decode with the full model; pin it to `tiny`/`base` (wake-phrase matching needs only rough text).

### 2. In-engine wins (no engine change) — verified

- **`flash_attn(true)`** — one line in `Transcriber::load`. Measured upstream (PR #2152, M1 Pro Metal): encoder ~13–21% faster (small 217→184 ms, medium 597→518 ms), decoder ~10–20%; upstream has since made FA the **default-ON** (proven in our vendored 1.8.3 source). One unconfirmed anecdote of FA slightly changing Japanese output (issue #3020) → gate on a Polish A/B. We don't use DTW timestamps, so FA's DTW conflict is moot.
- **`audio_ctx` right-sizing** — the Fast profile encodes a 30 s-padded window for 14 s of audio. ggerganov: 768 ≈ "2× faster encoder"; measured `(len/30)*1500+128` gave ~3.4× with WER unchanged (base.en). For 14 s: **832**. Live-only; batch keeps full context. Medium confidence on Polish-neutrality (tested on base.en only) → same A/B harness.
- **Unconditional live pin to `small`(-q8_0)** — the pin logic already exists behind `brain_live`; make it unconditional (config escape hatch). `small` is ~5.5–8× cheaper per tick than large-v3; on M2 Pro+ it clears the 4.7× RTF the loop demands, large-v3 cannot (saturation).
- **VAD tick gate** — Silero costs <1 ms per 30 ms chunk on one CPU thread (README-verbatim); gate on "speech in the 3 s delta" + 1–2-tick hangover; the manual voice-capture flow bypasses the gate while armed (its budget is tick-counted). Live VAD **must stay CPU** (`vad.rs`: second ggml Metal context aborts). WhisperLiveKit marks running without VAD "NOT ADVISED"; whisper.cpp's own stream example has a VAD-gated mode.
- **Thermal governor + QoS** — `NSProcessInfo.thermalState` (`objc2-foundation`, additive feature; rule-7 guarded FFI): `fair` → tick 6 s; `serious` → tick 9 s + pause reactions scans; `critical` → suspend captions (recording + batch untouched). Tag the caption/reactions threads `UTILITY`/`BACKGROUND` (`pthread_set_qos_class_self_np`, symbol confirmed in vendored libc). Apple guidance: apps should sit at utility-or-lower ≥90% of the time.
- **Tick defers during a user turn** — the ASR tick should skip once while `user_turn_in_progress` (the P0.3 flag) is set on the local backend: ~5 lines, directly reduces the worst Metal co-residency spike.
- **Quantized models = RAM/disk win, not speed win, on Metal** (q8_0 safe rung; the only controlled study is English-only, tiny/base-class — Polish quant behavior needs the in-house A/B). Extend `model_filename` with quant variants regardless (the HF mirror already hosts them).
- **Keep beam-5 batch.** Beam-2 would be ~2.35× faster at <1.5 pp WER cost (single-source), but batch runs once and Polish is inflected — not the heat source; offer beam-3 as an opt-in "faster batch" later at most.

### 3. Batch default: `large-v3-turbo-q8_0` for Polish

Whisper-paper FLEURS Table 13 (verified against the PDF): Polish WER **tiny 45.6 / base 30.8 / small 14.7 / medium 8.0 / large 7.2 / large-v2 5.4**. OpenAI (verbatim): turbo "performs similarly to large-v2", degradation flagged for Thai/Cantonese, **not Polish**. So turbo ≈ **~5–6 Polish WER vs our default small's 14.7 — ~2.5× fewer errors** — with the large-v3 encoder but a 4-layer (8× lighter) decoder: batch wall-clock ≈ medium-class, RAM ~1.2 GB (q8_0). BIGOS V2 (NeurIPS 2024, Polish-specific) confirms the monotone ladder and warns Whisper hallucinates more on conversational Polish (expect meeting WER worse than FLEURS). **No usable Polish distil-whisper exists** (official distils EN-only; the one community PL distil has no WER, no ggml, ~0 adoption). Direct turbo-Polish WER is unpublished — inference via large-v2, flagged honestly; the A/B harness closes it.

### 4. Engine landscape (the Brain v2 horizon)

- **parakeet-tdt-0.6b-v3** (NVIDIA, CC-BY-4.0, model-card-verified): 25 languages **incl. Polish (FLEURS 7.31 / MLS 7.28)**, RTFx 3,332 (datacenter); Mac ports: FluidAudio CoreML/ANE (Apache-2.0, README claims **~190× RTF on M4 Pro**, 40+ shipping apps incl. VoiceInk), parakeet-mlx (955★, no formal releases), and — decisively — **the sherpa-onnx int8 export runs through the crate already in our lockfile** (`OfflineTransducerModelConfig`, `model_type = "nemo_transducer"`, documented with a Parakeet example in the 1.13.3 crate source). Caveats: int8 missing-words bug #2605 fixed via PR #2606 — **whether the fix is in 1.13.3's bundled C lib is unverified** (spike catches it); auto language-ID only (PL↔EN code-switching unmeasured); quality gap to whisper's best is ~1.35× (paper-verified large-v2 5.4 vs 7.31) — so **whisper stays the batch authority; parakeet is the live-path candidate**, better-than-today's-live-small AND off Metal entirely.
- **WhisperKit / argmax-oss** (MIT, Swift/CoreML): the ANE escalation path — their (Argmax-authored, English-only; accurately cited but not independent) paper measures decoder **1.5 W → 0.3 W on M3 ANE**, ~0.45 s streaming latency, production LocalAgreement streaming. Polish WER of the compressed CoreML variants is unpublished. Hold as escalation if CPU int8 parakeet still runs hot.
- **Apple SpeechAnalyzer (macOS 26): no Polish** — two independent locale lists (42 locales, no `pl_*`) + a forum report that `supportedLocale(equivalentTo:)` returns nil for Polish; Argmax benches its quality between base.en and small.en anyway. Disqualified until Apple adds Polish (one Swift call on a macOS 26 Mac re-checks).
- **Dismissed** (no Polish or wrong direction): sherpa streaming zipformers, Voxtral Mini, kyutai STT, Vosk, moonshine, OWSM.
- **Market convergence validates the split**: MacWhisper (whisper.cpp + WhisperKit + parakeet), VoiceInk (whisper.cpp + parakeet via FluidAudio), Whisper Notes (parakeet default). Nobody ships one engine for all paths.

### 5. Streaming architecture (the correct v2 live loop)

Fixed-window re-decode is the documented anti-pattern. **LocalAgreement-2** (UFAL; WhisperKit ships it in production): commit the longest prefix two consecutive decodes agree on, trim the audio buffer at committed segment boundaries, feed committed text back as the decoder prompt → each audio second decoded ~2× instead of ~4.7×, and captions stop flickering (a better substrate for the live buffer + Brain v2 L4's incremental bullets). SimulStreaming/AlignAtt (~5× faster still) needs per-step cross-attention access that whisper-rs doesn't expose — out of reach without a fork. The streaming rewrite is the right Brain v2 architecture but is **not needed to hit the heat target** — Phase 1 alone exceeds it.

## Fit with Murmur constraints

- **Local-first**: everything is on-device; model/quant/mlmodelc downloads are inbound-only via the existing `download_model_streaming` pattern. Zero egress change; ASR sits below the provider seam + redaction firewall.
- **SQLite-canonical / Obsidian**: untouched; parakeet's word timestamps map onto `Segment{start_s, end_s, …}`; the wall-clock Me/Others merge is engine-agnostic.
- **macOS**: thermal FFI follows rule 7 (guarded, graceful degrade); live VAD stays CPU (documented ggml abort); CoreML paths add first-run ANE-compile UX + notarization surface — that's why they're held as escalations.
- **Licensing**: parakeet weights CC-BY-4.0 (attribution line in About), sherpa/FluidAudio Apache-2.0, WhisperKit MIT — all AGPL-compatible.
- **RAM guard**: turbo-q8_0 (~1.2–1.5 GB) + small live (~0.9 GB) fits the post-OOM residency budget far better than large-v3 fp16 (~3.9 GB); live and batch transcribers are sequential by design (live loop self-terminates at Stop), so peak co-residency is brief. Parakeet int8 (~1 GB) must register with the RAM-refuse guard.
- **No new deps**: Phase 1 + 2 = zero new crates (cargo-feature flips + existing deps). FluidAudio/WhisperKit sidecars = new dep + helper, needs user approval.
- **CI honesty**: wiring is unit-testable headless; **every speed/watts/Polish-quality claim needs a real Mac** (sudo `powermetrics`, a loaded model, a fixed PL/EN WAV) — green `cargo test --lib` proves none of it.

## Options & tradeoffs

| Phase | Content | Size / risk | Expected win |
|---|---|---|---|
| **T0 measure** | `scripts/measure-live-power.sh` (powermetrics A/B protocol: cpu/gpu/ANE power + per-process energy) + per-tick decode-duration logging (counts/durations, no PII) + a fixed ~10-min PL/EN WAV `#[ignore]` A/B harness | S, none | The baseline every later change must beat; the project's first real ASR numbers |
| **T1 free wins** | `flash_attn(true)`; `audio_ctx=832` (Fast only); unconditional live pin to `small`; Silero tick gate (+hangover, manual-capture bypass); thermal governor + QoS tags; tick defers during user turn; wake-listener pinned to `tiny`/`base` | S–M, low (each piece revertible, config-gated) | Modeled 6–50× live compute cut — the heat fix |
| **T2 batch quality** | quant-aware `model_filename`; default `large-v3-turbo-q8_0` for pl/auto (RAM-checked; `small` fallback); A/B-gated | S–M, low | ~2.5× fewer Polish errors in the persisted transcript |
| **T3 parakeet live spike → adopt** | 1-day spike: sherpa-onnx `OfflineRecognizer` + parakeet-v3 int8 vs current live path on the same WAV (wall, watts, RSS, transcript diff incl. PL↔EN switching + silence) — adopt for live if numbers hold | M, medium (int8 fidelity, LID) | Live captions better than today's small AND off Metal (GPU freed for the brain LLM) |
| **T4 streaming rewrite** | LocalAgreement-2 live loop (grow-commit-trim), integrated with Brain v2 L4 (gatekeeper + incremental bullets read the committed stream) | L, medium | ~2× further redundancy cut; stable captions; the correct v2 substrate |
| **Held escalations** | whisper-rs `coreml` encoder (ANE), FluidAudio sidecar, WhisperKit batch | M–L | Only if T1–T3 measurements show Metal contention/energy still limiting |

## Recommendation & first step

**Adopt T0→T1→T2 now; run the T3 spike; fold T4 into Brain v2's L4.** The single cheapest verifiable first step is **T0**: add per-tick decode timing to `live.rs`, commit `scripts/measure-live-power.sh`, record one scripted 10-minute meeting twice (small vs large-v3 live) under `sudo powermetrics -i 1000 --samplers cpu_power,gpu_power,ane_power,thermal --show-process-energy`, and commit the numbers as the eval artifact. That one day of work turns every modeled number in this report into a measured baseline, and the same harness then gates FA, `audio_ctx`, turbo-q8_0, and parakeet — the same measure-first discipline the Brain v2 eval gate (L1.6) demands for retrieval.

## Open questions / what couldn't be verified

- **Actual watts on this hardware** for any configuration — no published source measures our exact path; only T0 answers it (needs sudo + a real Mac).
- Polish quality under: flash attention, `audio_ctx=832`, q8_0 quants, parakeet int8, PL↔EN code-switching — all unpublished; the A/B harness is the instrument.
- Whether sherpa-onnx **1.13.3**'s bundled C lib includes the parakeet int8 missing-words fix (#2605/PR #2606), and whether its static ORT can enable a CoreML EP.
- Direct large-v3-turbo Polish WER (inferred ≈ large-v2 5.4; the "large-v3 = 4.7" figure is vendor-blog-grade).
- whisper-rs `coreml` + `metal` feature coexistence in one binary (build-level yes per build.rs; not spiked).
- SpeechAnalyzer locale list on a live macOS 26 box (may grow in point releases).
- whisper-rs maintenance cadence (GitHub archived → Codeberg; 0.16.0 is 2026-03) — a mild strategic argument for the sherpa-onnx/parakeet seam.

## Measured results (2026-07-10 appendix — in-house, M-series dev Mac)

First real numbers from the harnesses this report demanded (`asr_ab_harness_from_env` + `parakeet_spike_from_env`, both shipped as `#[ignore]` env-driven tests; 131 s synthesized PL→EN→PL WAV, macOS `say`/Zosia — clean TTS, NOT meeting audio; post-#230 tree, so flash-attn ON + VAD-segmented batch):

| config | wall | RTF | Polish quality (same WAV, eyeballed diff) |
|---|---:|---:|---|
| small, Fast profile (today's live) | 0.77 s | ~170× | names garbled (Jakub→"jak ut", Łukasz→"półkarz", SQLCipher→"skłelcifer"), EN segment DROPPED, hallucination repeat-loops |
| small, Accurate (today's batch default) | 2.75 s | ~48× | same error classes as Fast |
| **large-v3-turbo-q8_0, Accurate** | **2.73 s** | ~48× | near-perfect PL (Jakub/Łukasz/Apple Silicon right), EN intact — **same batch wall as small, ~2.5× fewer errors** |
| parakeet-tdt-0.6b-v3 int8, CPU ×4 (off Metal) | 10.0 s | ~13× | better than small (names right, zero loops, clean PL↔EN auto-LID), below turbo ("Apple śliczon", "modelu ISP") |

Confirms the report's ladder (`small < parakeet < turbo`) and both engine recommendations: **(T2)** the turbo-q8_0 batch default costs the SAME wall as small on the VAD-segmented flash-attn batch path — the flip is now evidence-backed (cost: 874 MB download, ~1.2 GB RAM vs small's 470 MB); **(T3)** parakeet is a viable live-path engine at ~13× realtime on four CPU threads with Metal left entirely to the LLM, via the sherpa-onnx crate already in the lockfile (1.13.3 — no missing-words symptom on this WAV). Open: watts (T0 powermetrics protocol still pending, user-assisted), real-meeting audio, and PL WER on non-TTS speech.

## Sources

**Repo (symbols):** `transcribe/live.rs` (`TICK`, `WINDOW_SECS`, tick body, `accumulate_live_caption`, `step_manual_capture`), `transcribe/whisper.rs` (`Transcriber::load`, `build_params`, `TranscribeQuality`, batch consts), `transcribe/vad.rs` (CPU-only rationale), `transcribe/model.rs` (`model_filename`, `ensure_model`), `audio/listener.rs` (`VoiceListener::start`), `commands.rs` (`live_model` pin, `restart_voice_listener`), `pipeline.rs` (`transcribe_stream`, `decode_windows`), `settings/config.rs` (`model_size`, `voice_trigger`), `Cargo.toml`/`Cargo.lock` (whisper-rs 0.16 / sherpa-onnx 1.13.3 / objc2-foundation); vendored: `whisper-rs-0.16.0/src/whisper_ctx.rs` (flash_attn default), `whisper-rs-sys-0.15.0/whisper.cpp` (1.8.3, FA default true; coreml build.rs), `sherpa-onnx-1.13.3/src/offline_asr.rs` (nemo_transducer example).

**External (fetched):** whisper.cpp PR #2152 + discussion #2155 + issues #3020/#1855/#1307/#2126/#89 + discussion #297 + README; HF ggerganov/whisper.cpp file tree; arXiv 2503.09905 (quant study); openai/whisper discussion #2363 + HF whisper-large-v3-turbo; arXiv 2212.04356 (Whisper paper, FLEURS Table 13 extracted); BIGOS V2 (NeurIPS 2024); HF nvidia/parakeet-tdt-0.6b-v3; k2-fsa sherpa docs (nemo transducer models) + issue #2605; FluidInference/FluidAudio + parakeet-tdt-0.6b-v3-coreml; senstella/parakeet-mlx; arXiv 2507.10860 (WhisperKit, Argmax-authored); argmaxinc.com/blog/apple-and-argmax; Apple docs (ProcessInfo.ThermalState, QoS energy guide, ane-transformers); Anton Gubarenko iOS-26 SpeechAnalyzer guide + Apple forums 790108/806765; ufal/whisper_streaming + SimulStreaming; QuentinFuxa/WhisperLiveKit; snakers4/silero-vad; justvoice.ai + getspeakup + mac-whisper-speedtest + dicta.to benches (blog-grade, flagged); 9to5Mac (MacWhisper 13); mistral.ai/voxtral + HF kyutai (dismissals); local `man powermetrics`.
