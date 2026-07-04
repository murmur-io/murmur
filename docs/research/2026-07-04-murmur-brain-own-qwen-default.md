<!-- Generated 2026-07-04 via /research (murmur-researcher fan-out, 4 angles: licensing/fine-tune, runtime concurrency, architecture fit, product strategy). Pricing/version/license = point-in-time. -->
# Research: "Our own Qwen" as Murmur's default on-device model (light/heavy) + gating realtime model reactions on it

## TL;DR / Verdict

**The idea is sound — in the HYBRID shape, and with an immediate license fix.**

1. **🔴 LANDMINE (act regardless of everything else):** the registry's small model `qwen2.5-3b` is under the **Qwen Research License — NON-COMMERCIAL ONLY**. It cannot be curated/defaulted in a commercial app. Two agents verified this independently against the HF LICENSE file. **Swap it for Qwen3-family (Apache 2.0) now.**
2. **The right shape is B — hybrid, branded "Murmur Brain":** cloud (consented + redacted, as today) keeps writing the polished NOTE; a light on-device model (Qwen3-1.7B/4B, Apache 2.0) becomes a default-offered onboarding download that exclusively powers **realtime private reactions** (The Whisper) + local layers. Full-local-default for notes (shape A) is rejected today: 3–4B note quality on long/Polish transcripts is unproven and gambles the core artifact.
3. **Gating realtime reactions on model presence** = exactly the house pattern (`should_auto_index`-style `flag && model_present`), BUT as a **new toggle** — today's `realtime_reactions` flag gates only voice-action wake-word dispatch, don't overload it.
4. **Fine-tuning ("tuned by us"): defer.** Evidence says a tuned small extractor would win (LoRA Land, NuExtract, UniversalNER), but measure the prompt-only baseline first. When tuning: distill from Apache/MIT teachers ONLY (Claude/GPT ToS both prohibit training competing models on their output), ship merged GGUF (mistralrs LoRA adapters don't support qwen archs).
5. **Runtime verdict:** viable on 16 GB+ (the whole Mac lineup ships 16 GB since Oct 2024); 8 GB tier gets a 1.5–1.7B model or the AFM sidecar. Residency: **load at recording start, NEVER unload** (mistral.rs has a documented history of leaking on model drop). Expected per-extraction wall-clock under whisper contention: ~3.5–7 s on base M-chips with output capped at ~100 tokens — fine for "just now" whisper cards, not for sub-second UX.

## Co już mamy (from the repo — grep symbols, line numbers drift)

- **Registry** (`reason.rs:70-101`): `bielik-11b-v3` (~6.7 GB, Apache 2.0 ✓), `qwen3-14b` (~9 GB, Apache 2.0 ✓), `qwen2.5-3b` (~2 GB, **research license ✗**). Download-on-demand; downloader has **no resume, no SHA-256** (`reason.rs:201-253`) — not default-grade.
- **Roles resolver** (`summarize/roles.rs`): Notes/Ask/Live with per-role connection/model/effort keys — light-for-Live / heavy-for-Notes is *already config-expressible* (`reason.rs local_cached` honors role model id).
- **THE WALL:** `provider_for` **refuses** reasoner-only targets (`summarize/mod.rs:114-128`) — the on-device model **cannot write the note today**; the FE hides Local on the Notes row on purpose (`ai-role-rows.component.ts:92-99`). Only Notes *pre-analysis* runs on the reasoner (`pipeline.rs:917-918`).
- **Single-slot model cache** (`reason.rs:300,412-426`): alternating light/heavy reloads multi-GB weights every swap — needs a path-keyed map for dual residency.
- **`realtime_reactions`** (`settings/config.rs:253`, `transcribe/live.rs:307-309`): gates ONLY the wake-word voice-action dispatch. The FE toggle is "In-meeting voice assistant" with a cloud-consent banner.
- **Deterministic realtime floor already ships:** `proactive.rs` — zero-LLM entity/commitment/FTS recall cards over live captions, ≤1 card/120 s, visibility-gated. The Whisper is an LLM upgrade of this surface, not a new pipeline.
- **Presence-gating idiom:** `should_auto_index(flag, model_present)`, `brain_model_present`, `embed_model_present`, `{status:"model_missing"}` degradation.
- **Onboarding already downloads a model** (whisper step, `onboarding.component.ts:18`) — the first-run-download UX pattern exists; "Fully local" posture currently means Ollama.
- **Privacy structurally true for local:** `MistralReasoner` has zero egress code; redaction/ledger wrap only cloud-classified ids. **Caveat:** `egress_is_cloud` defaults unknown ids → cloud (`summarize/mod.rs:73`) — a new local connection MUST get an explicit `=> false` arm or the Privacy Receipt lies cloud-ward.
- **Eval harness exists, never run** (`eval/bakeoff.rs`); GGUF path abandoned grammar-constrained decode (schema-in-prompt + `extract_first_json`, `reason.rs:14-21`).
- **AFM seam built, sidecar deferred** (`reason/afm.rs`): Apple's ~3B on the ANE = zero download, zero Metal contention, macOS 26+.

## Findings (per angle, condensed)

### 1. Licensing & model choice (confidence: HIGH — primary sources)

| Model | License | Verdict |
|---|---|---|
| Qwen2.5-3B-Instruct | **Qwen Research License — non-commercial only** | ✗ REMOVE from registry |
| Qwen3 dense 0.6B–32B (incl. 1.7B, 4B-Instruct-2507) | **Apache 2.0** | ✓ ship, tune, rename freely |
| Bielik v3 1.5B/4.5B/11B (speakleash) | **Apache 2.0** | ✓ Polish-native alternates |
| Gemma 3 4B | Google ToU w/ remote-restriction right + EULA pass-through | ✗ (ergonomics) |
| Llama 3.2 3B | "Built with Llama" + name-prefix tax; no Polish | ✗ |
| Phi-4-mini 3.8B | MIT, Polish listed | fallback (arch unverified) |

**Recommended pair:** light = **Qwen3-1.7B** (thinking hard-off; ~1.1 GB Q4; `qwen3` arch parses on pinned mistralrs 0.8.1) · heavy = **Qwen3-4B-Instruct-2507** (non-thinking, 262k ctx, Q4_K_M 2.50 GB) · Polish alternates Bielik 1.5B/4.5B. White-label as **"Murmur Brain"** (precedent: Zed "Zeta" = renamed Qwen2.5-Coder fine-tune; competitor ships a Qwen3-1.7B fine-tune as its default local model). Apache = attribution in About/Settings, no marketing-copy obligations.

**Fine-tune evidence:** LoRA-tuned small models beat GPT-4 by ~10 pts avg on narrow tasks (LoRA Land, arXiv 2405.00732); NuExtract (Phi-3.5-mini tune) beats GPT-4o on JSON extraction; UniversalNER +7–9 F1 over ChatGPT. Our light task (tiny fixed schema, 2–3-sentence windows, PL+EN) is exactly that regime — but **prompt-only baseline first**. Training data: distill from Qwen3-large/DeepSeek (MIT, explicitly permits distillation) — **never Claude/GPT** (Anthropic Commercial Terms §D.4 and OpenAI ToU both prohibit training competing models). Must ship **merged GGUF**: mistralrs 0.8.1 GGUF+LoRA adapters support only llama/phi3 archs (verified in pinned crate source).

**Distribution:** bundling is dead (GitHub release assets hard-capped at 2 GiB; DMG+model exceeds it) → first-run download (industry norm; HF rate limits a non-issue for one-file GGUF). Downloader needs **Range resume + pinned SHA-256** before any "default" story.

### 2. Runtime concurrency (confidence: MED-HIGH; all load-bearing numbers need a real-Mac spike)

- **whisper.cpp + small LLM coexistence is field-proven** (talk-llama in-process on Metal since 2023; Metal time-slices command buffers; no starvation reports found — but no rigorous measurement of our exact overlap pattern exists either).
- **Latency** (derived from llama.cpp #4167 anchors): 3B Q4 extraction call (~300 new prompt tok + 150 JSON tok) ≈ 6.5 s isolated / 9–12 s under contention on base M1; ~3.5–7 s on M2–M4 base; ~1.5–4 s on Pro chips. Levers: cap JSON output at ~80–100 tok (decode dominates); **prefix caching is already default-ON in mistralrs 0.8.1** (verified in vendored source — constant system prompt prefills once); a 1.5–1.7B model halves everything.
- **RAM:** 16 GB = honest floor for the 3–4B plan **because the user is also in a Zoom/Meet call** (Murmur stack ≈ 4.3 GB + call 1–2 GB + OS). 8 GB → 1.5B-class or AFM or off. GGUF loads are full RAM copies (candle `read_exact`, no mmap) → eviction = real SSD swap (25→2 tok/s cliff).
- **Residency:** load light model at recording start, **never drop** — mistral.rs issues #723/#865 document leaks on model drop (0.2.x era; 0.8.1 status unverified → 30-min RSS spike decides). First-ever run pays Metal shader compile (`MISTRALRS_METAL_PRECOMPILE=0`) — pre-warm in onboarding, never mid-meeting.
- **Prior art:** nobody ships live in-meeting on-device LLM extraction (closest competitor runs local live *transcription* + post-call local summaries with a ~1.1 GB 1.7B fine-tune) → **differentiated, not commodity**. Their choice of 1.7B over 3B+ is a signal.
- **Battery/thermals:** ~9% throttle after 15 min continuous on fanless Air (only credible datapoint); our duty-cycled pattern (~3–6 s burst / 15–30 s) is survivable; `powermetrics` on a real Air is the honest bar.

### 3. Architecture fit (confidence: HIGH — every seam read)

**Verdict: fits — the seams were grown toward this.** Change-map:

| Seam | Change | Size |
|---|---|---|
| `summarize/local.rs` (new) | `LocalSummarizerProvider` over shared local reasoner (trait has NO streaming — friendly) | M |
| `summarize/mod.rs` | `CONN_LOCAL` build arm in `provider_for` (stop refusing); `egress_is_cloud`: `CONN_LOCAL => false` (**same PR or the Privacy Receipt lies**) | S |
| `summarize/roles.rs` | Split `is_reasoner_only` → `builds_no_provider()` vs `is_agentic_eligible()` (three conflated identities diverge) | M |
| `reason/mistral.rs` | Process-level `path → Arc<Model>` weight cache (shared by ReasonerCell + provider; solves dual residency in one stroke) | S–M |
| `reason.rs` | Single slot → path-keyed map (cap 2–3) | S |
| registry/commands | Combined-residency RAM guard (min_ram_gb is per-model, lies for co-residency) | S |
| FE settings | "Murmur on-device" in Default-AI select; Notes-row unhide **later, after quality evidence** | S |
| Reactions toggle | **NEW flag** (not `realtime_reactions`); backend gate = `flag && model_present`; FE disabled-with-download-nudge | S |
| Onboarding | "Fully local"/brain posture → light-GGUF download (whisper-step pattern) → set default **on success only** | M |
| Migration | **No sentinel flip** — a fresh install has no GGUF; default flips only post-download; existing users get a nudge card. local→cloud fallback on missing model is FORBIDDEN (silent egress) | S |
| Reviews | lock-security + adversarial re-bless (egress classifier + receipt touched) | — |

### 4. Product strategy (confidence: MED-HIGH)

- **Quality reality:** HHEM leaderboard shows small models are NOT hallucination liabilities on short-doc summarization (Qwen3-4B 5.7% vs Claude Opus 4 12.0%) — but long-transcript minuting structure/recall is where they degrade; practitioner sweet spot for messy transcripts ≈ 14B. Polish quality of Qwen3-4B notes: completely unmeasured. → **notes stay cloud-by-default; local notes = opt-in choice gated on bake-off evidence.**
- **Demand:** privacy demand is profession-anchored (ABA Formal Opinion 512 makes "AI never leaves the Mac" collapse a lawyer's compliance analysis to zero); realtime in-meeting assistance demand proven by a cloud competitor's $5.2M ARR — whose 83k-user breach is the exact argument for local ("private whisper computed in the cloud" is self-defeating; far-side words are the legally hottest content).
- **Gating-as-unlock works** if soft-gated: show the deterministic recall card first (it ships today), then offer "unlock realtime reactions — one-time ~1–2.5 GB download" in-context. Hard gate with no floor = churn point. Apple normalized 7 GB of on-device models; our onboarding already downloads whisper.
- **#1 product risk is NOT the download — it's a noisy/slow whisper layer discrediting the flagship** (inherit `proactive.rs` throttle discipline: cooldowns, score thresholds, show-source-or-silent) **and the silent licensing bug of defaulting a research-licensed model.**

## Fit z ograniczeniami Murmur

- **Local-first:** strengthened — default install finally exercises the moat (our own audits' #1 finding: the moat ships dormant). Model download is inbound-only.
- **Provider seam + redaction:** preserved by construction; local exempt the same way loopback Ollama is. The `egress_is_cloud` arm is load-bearing for the Privacy Receipt.
- **SQLite-canonical / Obsidian:** untouched; whispers are ephemeral events (proactive-hint mold).
- **macOS/CI honesty:** `cargo test --lib` proves wiring only — **no forward pass has ever been verified on this project** (`mistral.rs` header says so). Latency/contention/leak/battery/PL-quality all need a signed real-Mac spike.

## Opcje i tradeoffy

- **A. Registry swap + license fix (S, days, DO NOW):** remove `qwen2.5-3b` → add `qwen3-1.7b` (light) + `qwen3-4b-instruct-2507` (heavy); wire `enable_thinking=false`. Zero risk; kills the liability.
- **B. Hybrid default "Murmur Brain" (M, the recommendation):** A + default-grade downloader (resume+SHA-256) + onboarding brain-download step + two-slot model cache + NEW presence-gated reactions toggle + The Whisper riding `Role::Live` local with `proactive.rs` as floor. Cloud keeps notes.
- **C. LocalSummarizerProvider — local notes as a CHOICE (M, after B):** the change-map above; unhide Notes row only after a real-Mac bake-off (local vs cloud on the same real meetings, PL included).
- **D. `murmur-extract-1.7b` fine-tune (L, gated on B's numbers):** unsloth LoRA on Qwen3-1.7B, distilled from Apache/MIT teachers, merged→GGUF→own HF repo. Pre-justified by literature IF the prompt-only baseline disappoints.
- **E. Full-local default for notes (rejected today):** revisit only if C's bake-off shows local within a blind-preference margin of cloud.

## Rekomendacja i pierwszy krok

**A natychmiast; B jako kształt docelowy; C/D gated na pomiarach.** To bezpośrednio realizuje intencję użytkownika ("nasz Qwen zawsze dostępny, light do realtime + heavy do analiz, realtime reactions włączane gdy jest") — w kształcie, który nie ryzykuje jakości notatki i nie łamie licencji.

**Smallest verifiable first slice (jeden dzień + jeden real Mac):**
1. Registry swap (static table + tests) + `enable_thinking=false`.
2. Real-Mac spike: download Qwen3-1.7B, run the EXISTING `extract_fact_candidates` prompt over 20 PL + 20 EN transcript windows **while a recording with live captions is running** — measure JSON validity, triple quality, per-call wall-clock, `EVENT_LIVE_CAPTION` inter-tick latency (contention signal), RSS with a Meet tab open, `powermetrics`.
3. 30-min leak spike: 10× load/drop of a 2 GB GGUF on 0.8.1 watching RSS (decides if unloading is ever allowed).
4. 1-h bonus: retry `Constraint::JsonSchema` with the tiny 3-field facts schema on 1.7B (the overflow was Bielik-11B + big schema; grammar-constrained decode may just work and buys the reliability a fine-tune otherwise buys).

That afternoon of evidence answers the only questions research can't: is prompt-only 1.7B extraction good enough (→ D go/no-go), and does live co-residency actually hold on base hardware (→ B's RAM/latency gates).

## Otwarte pytania / czego nie udało się zweryfikować

- **No verified forward pass ever** through MistralReasoner on this project — all inference claims are estimates until the spike.
- mistralrs 0.8.1 drop-leak status (issues are 0.2.x-era); prefix-cache HIT behavior across separate calls (default-on verified, hit not).
- True overlapped whisper+LLM degradation ratio — no published measurement of our exact pattern anywhere.
- Qwen3-1.7B/4B **Polish** extraction/note quality — unmeasured; the bake-off decides.
- Phi-4-mini GGUF arch on mistralrs (`phi3` path — plausible, unverified); HF CDN Range support (verify with `curl -r`).
- Whether users who selected whisper `large-v3` saturate the GPU alone (live tick may need force-pinning to `small` when reactions are on).
- unsloth merged-GGUF export API (doc 404'd; llama.cpp conversion route is the fallback).

## Sources

**Licenses (primary):** Qwen2.5-3B LICENSE (research, non-commercial) hf.co/Qwen/Qwen2.5-3B-Instruct · Qwen3 Apache 2.0: qwenlm.github.io/blog/qwen3 + hf.co/Qwen/Qwen3-1.7B + hf.co/Qwen/Qwen3-4B-Instruct-2507 · Bielik v3 Apache 2.0: hf.co/speakleash/Bielik-{1.5B,4.5B,11B}-v3.0-Instruct · Anthropic Commercial Terms §D.4 (no training competing models) · DeepSeek-R1 MIT (distillation explicitly permitted).

**Runtime:** llama.cpp Apple-Silicon benchmark #4167 (latency anchors) · whisper.cpp talk-llama (coexistence prior art) · mistral.rs issues #723/#865 (drop leaks) · vendored mistralrs-0.8.1 source: `gguf/mod.rs:16-33` (arch enum), `pipeline/gguf.rs:488-492` (LoRA archs), `gguf.rs:59-62` (prefix cache ON), `model.rs:639` (enable_thinking) · candle-core `gguf_file.rs:74` (no mmap) · Apple Foundation Models research (3B on ANE).

**Product:** Vectara HHEM hallucination leaderboard · Hyprnote/anarlog Launch HN (Qwen3-1.7B fine-tune default; hyprnote.com → char.com pivot observed) · ABA Formal Opinion 512 · Cluely (Wikipedia + breach coverage) · Zed Zeta blog (white-label precedent) · MacRumors 16 GB floor Oct 2024 · 9to5mac Apple Intelligence 7 GB · GitHub release 2 GiB cap docs · HF rate-limit docs · LoRA Land arXiv 2405.00732 · NuExtract hf.co/numind/NuExtract-1.5 · UniversalNER arXiv 2308.03279 · AMI Corpus CC BY 4.0.

**Key code:** `reason.rs:70-101` (registry) `:300,412-426` (single-slot cache) `:201-253` (downloader) `:14-21` (schema-in-prompt) · `facts.rs:214-287` (extractor) · `reason/mistral.rs` (honest-scope header; sync bridge) · `summarize/roles.rs:104-113,211-247` (resolver + is_reasoner_only) · `summarize/mod.rs:66-75,114-128,153-276` (egress classifier, provider_for refusal, wrap/sink) · `pipeline.rs:769-843,917-918` (Notes pipeline + receipt + pre-analysis) · `transcribe/live.rs:13-28,116,307-309,466-478` (live tick, should_dispatch, cloud-only agentic gate) · `proactive.rs` (deterministic floor) · `settings/config.rs:26-30,253,406` · `onboarding.component.ts:18-56` · `ai-defaults-block.component.ts:150-252` / `ai-role-rows.component.ts:92-112,214-279` · `eval/bakeoff.rs` (unrun harness) · `reason/afm.rs` (ANE seam).
