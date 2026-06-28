<!-- Generated 2026-06-28 via /research (murmur-researcher fan-out). Pricing/funding/version/model = point-in-time. Updates docs/research/2026-06-28-local-model-voice-decision.md with the delivery decision + the "how others do it" reference class. -->
# Research: how to DELIVER + run the local reasoning brain (Phase 3b) — and how other apps do it

## TL;DR / Verdict
**Embed the inference engine in-process and download the GGUF on first run** — exactly what Murmur already does for Whisper + diarization, and exactly what the closest peer (**Hyprnote**) and the standalone-app winners (GPT4All, LM Studio) converge on. Adding a third in-process runtime is the *same shape* of work we've shipped twice; it is NOT a new architectural category.

Two findings sharpen the prior decision doc and are the real news:
1. **mistral.rs is candle-backed, NOT onnxruntime** → it does **not** collide with sherpa's static ORT. The "2×-ONNX build risk" is real **only** for the GLiNER name-redactor (`gline-rs → ort`), a *separate* decision. This materially de-risks the reasoner build.
2. **The closest peer ships a TINY ~1.7B fine-tuned model, not a 14–30B general one.** Hyprnote (Tauri+Rust, YC S25) runs **HyprLLM = a Qwen3-1.7B fine-tune**, llama.cpp grammar-constrained, lazy-load+auto-unload. This opens a genuine fork in the model-size decision and directly validates your "maybe our own fine-tune" instinct.

**"Require Ollama" is the dominant pattern — but only for *plugins / BYO-LLM tools*, the wrong reference class for us.** Murmur is a standalone app that owns the whole UX → the embed-and-download camp is ours.

**First step is a BUILD-PROOF spike, not a model spike.**

## Co już mamy (z repo)
- Two in-process native ML runtimes already statically link: `whisper-rs 0.16` (Metal) + `sherpa-onnx 1.13` (its own **static onnxruntime**) — `Cargo.toml`. A third in-process LLM runtime is the same category of work.
- **Download-on-first-run is the shipped pattern** — whisper GGUF / VAD / diarization ONNX fetch once into `~/Library/Application Support/MeetNotes/models` (`transcribe/model.rs:56-85`). A reasoner GGUF follows it verbatim.
- The reasoner seam is merged + prod-inert: `LocalReasoner` trait + `structured(system,user,schema)` + `StubReasoner` + the brace/escape-aware `extract_first_json` (`reason.rs:21/31/92/44`). Real impl = one-line swap. Flywheel table live (`correction_log`, `storage/db.rs:259`). Provider seam `complete()` (`summarize/provider.rs:46`) is where a 4th in-process provider plugs in, upstream of the redaction firewall (zero egress for its decisions).

## Findings — how other apps deliver a local LLM (every claim a fetched URL, 2026-06-28)

Grouped by the actual delivery mechanism, because that's our decision axis.

### Camp A — REQUIRE a user-installed daemon (Ollama / LM Studio). The norm for **plugins + BYO-LLM tools**.
- **Obsidian Copilot** — no bundled model, no embedded engine; the user installs + runs Ollama/LM Studio separately, pulls a model, starts the server, then "Add Custom Model". Daemon must stay running. [1]
- **Reor** — Ollama frontend; models pulled *through* Ollama. **Repo archived 2026-03-07, read-only** — a cautionary signal that BYO-Ollama PKM isn't thriving. [2][3]
- **Meetily** — the direct meeting-notes peer: whisper.cpp embedded for ASR, but **summarization hard-depends on your local Ollama**. [4]
- **Anarlog** (open-source Granola alt) — ASR local in-process; **summaries default through Ollama**. Markdown + SQLite, architecturally close to us, but pushes the LLM onto Ollama. [5]
- **Khoj** — current docs tell you to run llama-cpp-server/Ollama/vLLM and point Khoj at it (but see the counter-example below — Khoj is a hybrid). [6]

### Camp B — EMBED the engine in-process; download GGUF on first run. The **standalone-app winner**. ← Murmur
- **GPT4All** — llama.cpp submoduled *into the app*; in-app hub shows **per-model RAM (4–16GB) + file size + quant**; downloads GGUF locally, no Ollama. [7]
- **LM Studio** — bundles **llama.cpp + Apple MLX** in-process (MLX ~30–50% faster on Metal), HW-compatibility guesser, *and* doubles as the `:1234` daemon others depend on. [8]
- **Smart Connections** (Obsidian) — the in-process embeddings winner: transformers.js in Electron, auto-downloads a ~25MB MiniLM, **"Zero setup. No API key."** (embeddings not generation, but the canonical bundle+download UX). [9]
- **Khoj (the counter-example to its own docs)** — historically embeds `llama-cpp-python`, auto-downloading a default GGUF; the embedded path is default-sticky. So Khoj is a **hybrid**: embedded-download by default, BYO-server optional. [6]

### Camp C — BUNDLE the daemon *binary* inside the app (ship it; don't make the user install it).
- **Msty** — bundles renamed Ollama executables per release; one-click, terminal-free GGUF download. Ollama's ecosystem without the install friction, at the cost of shipping a subprocess. [10]
- **Jan** — *(corrected by the fact-check: NOT an in-process library)* v0.8.0 runs a bundled, managed `llama-server --models-preset` **router subprocess** with a HuggingFace hub, colored hardware "fit pills", per-backend variants. Bundled (no user Ollama) but subprocess, so architecturally Camp C, not B. [11]

### The decisive peer — **Hyprnote** (fastrepl, YC S25) — near-identical stack
- **Tauri + Rust + TS**, local-by-default, **Rust-native inference (explicitly NOT Ollama)**: Whisper (whisper.cpp + Cactus in-process, ModelDownloadManager) + **HyprLLM**. [12][13][14]
- **HyprLLM = a Qwen3-1.7B fine-tune**, llama.cpp **grammar-constrained**, a ModelManager that **lazy-loads + auto-unloads** to manage RAM. [15]
- A third-party blog calling Hyprnote "BYO-Ollama" is **refuted by primary sources** (the "trust code not docs" correction). [16]

**Adversarial fact-check corrections (applied above):** Jan moved B→C (managed subprocess, not linked library); the genuine in-process exemplars are **GPT4All, LM Studio, Hyprnote**. The "no 2×-ORT", the #2125 panic, the Bielik facts, and the mistral.rs in-process path (official `gguf_locally` example, no server) all **held up** under refutation. `llama-cpp-2` exact version was over-precise; the recommendation doesn't rest on it.

## Fit z ograniczeniami Murmur
- **Local-first:** the brain runs on-device, zero egress for its decisions — Camp B is the only camp that needs no second process and no user setup. ✅
- **No-Ollama-hard-dependency:** Camp B satisfies it by construction; Camp A violates the *spirit* (and Reor's archival is the cautionary tale). ✅
- **macOS-first / our stack:** Hyprnote proves the exact Tauri+Rust+in-process path ships. ✅
- **Provider seam + redaction firewall:** a `LocalLlmProvider` plugs into `complete()` upstream of `RedactingProvider`. ✅
- **Download-on-first-run** is already our proven mechanism (`transcribe/model.rs`). ✅

## Opcje i tradeoffy

| Axis | Option | Verdict |
|---|---|---|
| **Runtime** | **mistral.rs** (candle/Metal, llguidance constrained decoding, LoRA, MIT) | **PICK.** candle ≠ ORT → no sherpa collision. In-process `gguf_locally`, no server. |
| | llama-cpp-2 (llama.cpp Rust bindings) | Solid fallback; what Hyprnote/GPT4All/LM Studio use under the hood. Keep as plan B. |
| | candle direct / mlx-rs | Lower-level; mistral.rs already wraps candle. MLX faster on Metal but less mature in Rust. |
| | Ollama/LM Studio as **optional** | Allowed as a *bonus* path (use if present), never required. |
| **Delivery** | **Download-on-first-run** (our whisper pattern) | **PICK.** No 9–20GB DMG; RAM-gated model picker like GPT4All/Jan. |
| | Bundle GGUF in DMG | Rejected — hostile download size. |
| **Model (point-in-time, spike-pinned)** | **Bielik-11B-v3.0-Instruct GGUF** (Llama-arch, Polish-native, Q4_K_M ≈6.7GB, tool-call template) | Safe default for Polish + mistral.rs arch-parse + no-panic. |
| | Qwen3-14B (multilingual), Qwen3-32B (RAM-gated opt-in) | Alternatives; **avoid qwen35/qwen3vl** (mistral.rs #2125 `unwrap()` on unknown GGUF arch = a no-hard-crash-rule violation — pin the arch). |
| | **Tiny fine-tune (1.7–4B), Hyprnote-style** | **The open strategic fork** — small/fast/cheap + domain-tuned on our PL-meeting flywheel vs a big general reasoner. Validated by the closest peer. |

## Rekomendacja i pierwszy krok
1. **BUILD-PROOF spike first (not a model spike).** In an isolated worktree (like the sqlite-vec spike): add `mistralrs`, run the official `gguf_locally` example with a small pinned-arch GGUF, and confirm it **links + runs in-process alongside sherpa's static ORT + whisper.cpp on macOS**, bundle-size + compile-time acceptable. This is the one thing that can't be assumed. If it links clean → wire `LocalReasoner` → real impl behind the merged seam.
2. **Deliver via download-on-first-run** with a RAM-gated model picker (GPT4All/Jan UX). Default to a small/medium model; 32B is opt-in.
3. **Resolve the model-size fork with evidence**, not taste: start with an off-the-shelf model (Bielik-11B or a 4B) for the structured pre-analysis/tool-call job; let `correction_log` accumulate; only invest in a Hyprnote-style fine-tune once the flywheel shows the off-the-shelf model is the bottleneck.

## Otwarte pytania / czego nie zweryfikowano
- mistral.rs in-process **bundle-size + compile-time alongside our existing static runtimes** on macOS — the spike answers this (the only real unknown).
- Whether a **1.7–4B** model is *good enough* for the decide-what-to-fetch / structured-extraction job, or whether the planning quality needs 11–14B — needs a real on-Mac eval (ties into the orchestration design + the bake-off).
- Polish quality of Bielik-11B vs Qwen3-14B for OUR tool-call/extraction prompts — on-Mac eval.

## Sources
[1] Obsidian Copilot local-model docs · [2] Reor README · [3] Reor archived 2026-03-07 · [4] Meetily (Zackriya) · [5] Anarlog (fastrepl) · [6] Khoj offline-chat docs + commit "Migrate to Llama.cpp" + issue #1253 · [7] GPT4All FAQ + models hub (per-model RAM/size) · [8] LM Studio docs + MLX-vs-llama.cpp Apple Silicon · [9] Smart Connections (brianpetro) README/site · [10] Msty offline-models docs · [11] Jan v0.8.0 local-engine docs (router subprocess) · [12] Hyprnote (YC S25) · [13] Hyprnote owhisper-server (DeepWiki) · [14] Hyprnote Show HN #44725306 · [15] HyprLLM = Qwen3-1.7B fine-tune, llama.cpp grammar-constrained, ModelManager lazy-load/auto-unload · [16] blog claiming BYO-Ollama (refuted by primary). All fetched 2026-06-28.
Code: `Cargo.toml` (whisper-rs/sherpa-onnx) · `transcribe/model.rs:56-85` · `reason.rs:21/31/44/92` · `summarize/provider.rs:46` · `storage/db.rs:259` · `docs/research/2026-06-28-local-model-voice-decision.md`.
