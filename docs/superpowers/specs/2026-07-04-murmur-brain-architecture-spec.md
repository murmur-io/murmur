<!-- Design spec v0.2, 2026-07-04. Distilled from: /dreaming (The Whisper prototype, docs/dreams/2026-07-04-the-whisper.md), /research (docs/research/2026-07-04-murmur-brain-own-qwen-default.md, 4-angle fan-out), and the product-shape conversation. v0.1 → v0.2: adversarially reviewed by three independent lenses (code-truth / performance / product+privacy), all PASS_WITH_FIXES; every blocking finding incorporated (see Review log, §10). Code citations are symbol-anchored (line numbers drift — grep the symbol). -->

# Murmur Brain — on-device AI architecture & performance spec

**Status:** v0.2 — adversarially reviewed (3 lenses, PASS_WITH_FIXES → fixes incorporated)
**Depends on:** `docs/research/2026-07-04-murmur-brain-own-qwen-default.md` (evidence base), `docs/dreams/prototypes/the-whisper/` (feel prototype for realtime reactions)

---

## 1. Summary

Murmur today has a split brain: **memory is local** (SQLite facts/graph/entities, FTS, candle embedder, NER) but **reasoning defaults to the cloud** (`brain_backend = Cloud` → `CloudReasoner` → the user's provider, default `claude_code`). The moat — a fully private, always-present intelligence — ships dormant.

This spec introduces **Murmur Brain**: a named, first-class on-device model stack (light + heavy + embedder), and reorganizes the product around **three postures** (Cloud / Hybrid / Fully Local). It unlocks one new flagship capability — **Realtime Reactions** (private in-meeting whisper cards: context / contradiction against the user's own meeting history) — which is **structurally local-only**: streaming a live call to a cloud model every ~15 s is too expensive, too slow, and egresses the far side's words (the legally hottest content we hold).

**Core mental model (the one sentence):**
> **Default AI (the provider picker) = who WRITES your content. Murmur Brain = what your Mac does by itself.** Transcription already lives there; realtime reactions, fact extraction and semantic search join it.

### Goals
1. Ship "Murmur Brain" as a branded, download-on-demand on-device engine pair (light ≈ Qwen3-1.7B, heavy ≈ Qwen3-4B-Instruct-2507; Bielik v3 as Polish-native alternates) — all Apache 2.0.
2. Ship **Realtime Reactions** gated on the light engine's presence — opt-in, throttled, deterministic-floor, zero-egress.
3. Make the **posture** the single user decision; per-role tinkering stays in "Customize per feature".
4. Fix the **license landmine**: retire `qwen2.5-3b` (Qwen *Research* License — non-commercial) from the curated registry, with an installed-base migration.
5. Close the existing privacy gap: when Brain Live is enabled, **fact extraction (meeting facts AND user-memory facts) stops egressing** (today both ride the cloud reasoner by default, redacted + consented).

### Non-goals (this spec)
- Local agentic tool-use loop ("Local agents") — the in-tree gate stays (agentic loop runs only on cloud-connection targets; the codebase's own warning: *local multi-step reliability unproven*). Brain Live's enablement card reserves the slot ("coming later").
- Fine-tuning (`murmur-extract-1.7b`) — deferred until the prompt-only baseline is measured (research §4; if tuned, distill only from Apache/MIT teachers, never Claude/GPT — ToS).
- Flipping the **Notes default** to local — Fully Local posture makes it a *choice*; any default flip is gated on a real-Mac bake-off.
- Mobile/AFM sidecar completion — AFM is designed-for as a future light-engine backend (`light_present() || afm_available()`), not built here.

---

## 2. Product surface

### 2.1 The three postures

A top-level Settings choice (and an onboarding step). One decision; everything else derives.

**Posture is a DERIVED display state, never a stored string** (review: product #4). It is a pure function over the actual dispatch keys (`role_*`, `brain_backend`, Brain Live flag), computed on read. The chooser writes presets; any hand-tuned combination that matches no preset renders **"Custom"**. A stored posture label that can disagree with dispatch is forbidden — a "Fully Local" banner over a hand-edited cloud Ask key would be a silent-egress lie. Reverse-mapping identity tests required.

| | **Cloud** | **Hybrid** ⭐ recommended | **Fully Local** |
|---|---|---|---|
| Notes (the artifact) | Default AI | Default AI | local heavy |
| Ask | Default AI | Default AI | local heavy |
| In-meeting `@brain` assistant | Default AI (agentic) | **Default AI (agentic, unchanged)** | local (deterministic floor — agentic stays cloud-only by the in-tree gate) |
| Facts (meeting + user-memory) | cloud reasoner (egress, redacted, consented) | **local light** | local light |
| Recall cards (deterministic, no LLM) | ✅ (ships today, no download) | ✅ | ✅ |
| Realtime Reactions (Brain Live) | ❌ → nudge | ✅ local light | ✅ local light |
| Semantic search / embedder | on-device (as today) | on-device | on-device |
| Egress | notes + ask + `@brain` + facts + pre-analysis | **notes + ask + `@brain` + pre-analysis** (facts local; see §3.4 for pre-analysis fate) | **zero** |
| Requires download | none | light (~1.1 GB) | light + heavy (~3.6 GB) |

Preset mappings (what the chooser actually writes):
- **Cloud** = today's defaults; Brain Live off.
- **Hybrid** = Cloud roles **untouched** + Brain Live enabled. **The Hybrid preset MUST NOT touch `role_live_*`** — `Role::Live` is the shipped in-meeting `@brain` assistant's dispatch role, and writing `role_live_connection=local` would flip its agentic loop to the deterministic floor (silently lobotomizing a flagship feature; review: code-truth #1). Realtime Reactions do NOT ride `Role::Live` — they ride the engine-class handle (§3.4).
- **Fully Local** = Notes/Ask connections → `local` (+ heavy model id) + Brain Live enabled. The `@brain` assistant on a local target degrades to the deterministic floor by the existing gate — the posture UI must SAY this ("in-meeting assistant runs simplified, fully local").
- Legacy configs map for display: `brain_backend=Cloud` → Cloud, `=Local` → Fully Local (or Custom if keys diverge), **`=Off` → Custom/Off — never displayed or re-saved as Cloud** (an Off user's Ask/@brain run the stub floor today; writing Cloud keys on first posture save would flip them to egress — review: code-truth #12).

**Invariant P1 — no silent cross-engine fallback.** If a feature's engine is absent/unloadable, the feature **degrades or disables with a nudge**; it never silently substitutes the other side:
- local-missing ⇒ NEVER cloud. With Brain Live on, the light-engine handle resolves **local-or-stub** (stub ⇒ empty extraction ⇒ feature visibly degraded + nudge) — never the cloud reasoner (review: code-truth #3, product #1). Presence is rechecked **per call**, not at enablement. RED test: Brain Live on + GGUF deleted ⇒ zero ledger rows, zero provider construction.
- cloud-configured ⇒ NEVER local (quality bait-and-switch).
- **Fully-Local note failure is specified UX, not an open question** (review: product #5): heavy absent/unloadable ⇒ the meeting lands in `MeetingStatus::Error` **with the transcript intact** (existing machinery; SQLite canonical, `resummarize` exists), plus a nudge offering exactly two explicit actions: *"repair/download the model"* or *"generate this one note with your Default AI"* (a consented one-shot choice, not a fallback). Optional: auto-resummarize when the model returns. Never queue-and-retry silently.

### 2.2 "Brain Live" — the enablement tier

Realtime Reactions (and later: local agents) are packaged as one named capability with a dedicated enablement card, NOT a bare toggle:

```
┌─ Enable Murmur Brain Live ────────────────────────────────┐
│  Your Mac gets its own always-present private brain.       │
│  One-time ~1.1 GB download from Hugging Face; after that,  │
│  nothing leaves your Mac.                                   │
│                                                             │
│  You unlock:                                                │
│   ⚡ Realtime reactions — live context & contradiction      │
│      cards matched against your own meeting history         │
│   🔒 [consented-cloud users:] Fact extraction moves fully   │
│      local (today it rides your cloud AI, redacted)         │
│      [non-consented users:] Fact extraction turns ON,       │
│      fully local (it is currently off without a model)      │
│   🧾 The egress ledger shows zero cloud calls for these     │
│      layers                                                  │
│   🔜 Local agents — coming later                             │
│                                                             │
│  ℹ Your Default AI still writes all notes.                  │
│  ℹ On fanless Macs under heavy thermal load, reactions      │
│    may pause to keep live captions smooth.                   │
│                          [ Enable Brain Live ]               │
└─────────────────────────────────────────────────────────────┘
```

Copy rules (review: product #9): the facts line is **conditional on the user's actual current dispatch** (telling a consent-refusing user "your facts were going to cloud" would be false and a self-inflicted trust wound — without consent, facts never extracted at all, fail-closed). "Nothing ever leaves" is qualified by the one-time HF fetch (a `murmur-io` mirror later also reduces download metadata exposure). The privacy-proof line references the **egress ledger** (which exists per-call), not the per-note Privacy Receipt (whose surface may not cover realtime layers).

- **Soft gate with a floor — two named tiers** (review: product #7, code-truth #5): **Recall cards** (deterministic, `proactive.rs`, zero-LLM, `proactive_hints_enabled` defaults ON) run everywhere with no download — they are NOT part of Brain Live and the matrix/copy must not claim them. **Realtime Reactions** (context + contradiction via the light engine) is the Brain Live tier. The enablement nudge is offered in-context after the user has seen a Recall card.
- The Default-AI picker is **untouched** — no local model appears there as a fourth peer option. Fully Local posture is where local notes live.

### 2.3 Anti-confusion copy

Anchor on transcription: *"Your notes are written by your Default AI (your choice). Realtime reactions run locally — streaming a live call to the cloud every few seconds would be slow, costly, and defeat privacy. Like transcription, this lives on your Mac."* No apology framing: realtime is not a downgraded cloud model, it's a capability cloud physically can't serve well.

---

## 3. Engine architecture

### 3.1 Engine classes and the registry

Two LLM classes + the existing embedder, behind the existing `LocalReasoner` / registry machinery:

| Class | Model (default) | Size (Q4_K_M) | Arch | Serves |
|---|---|---|---|---|
| **light** | Qwen3-1.7B (thinking hard-off) | ~1.1 GB | `qwen3` ✓ parses on mistralrs 0.8.1 | realtime reactions, fact extraction (meeting + user-memory), short classification |
| **heavy** | Qwen3-4B-Instruct-2507 (non-thinking) | ~2.5 GB | `qwen3` | local Notes/Ask (Fully Local posture), post-call analysis |
| PL alternates | Bielik-1.5B-v3 / Bielik-4.5B-v3 / Bielik-11B-v3 | 1.0/2.8/6.7 GB | `llama` | user-selectable per class |
| embedder | mmlw-retrieval-e5-small / multilingual-e5-small | ~0.5 GB | candle | unchanged |

**Registry changes (`reason.rs` `BRAIN_MODELS`):**
- **Retire `qwen2.5-3b`** (Qwen Research License, non-commercial — a live curation liability). **With an installed-base migration** (review: code-truth #6, product #6): a persisted `brain_model_id="qwen2.5-3b"` must not silently degrade to the stub. On first launch post-update: if the file is on disk, keep resolving it but hide the entry from the picker + show a nudge ("this model was retired for licensing; switch to Qwen3-1.7B — one-click download; optionally delete the old file"); if the file is absent, remap the selection to none + the same nudge. Registry tests that fixture the id get reworked in the same PR.
- ADD `qwen3-1.7b` + `qwen3-4b-instruct-2507` (+ optional Bielik 1.5B/4.5B). Each entry gains `class: Light | Heavy` and `sha256` fields.
- `enable_thinking=false` wired on the mistralrs path for qwen3 arch (the pinned crate exposes it).
- **`max_tokens` plumbing is a P1 prerequisite** (review: perf #2): today `MistralReasoner` sends requests with NO token cap or sampler params — the "≤100-token output" contention lever does not exist as a mechanism. Add a capped/structured request path (max_tokens ≈ 128, low temperature) through the `LocalReasoner` seam. In-flight generations are **non-cancellable** (engine channel runs to completion) — the sampler cap is the only real bound on damage per call; the spec's throttles prevent the *next* call, never the current one.

### 3.2 Dispatch: posture → roles → engines

No new dispatch layer. The existing three-view resolver (`summarize/roles.rs`) stays the single source of truth. Changes:

1. **Posture presets** write role keys as in §2.1 (a pure config-writing helper + FE); posture display is derived (§2.1).
2. **Predicate split** (`roles.rs`): `is_reasoner_only` currently serves two duties — provider buildability (`provider_for` refusal) and agentic eligibility (the `@brain`/Ask gates). Split into `builds_no_provider()` (`off`/`apple`) and `is_agentic_eligible()` (cloud connections only). Cloud-ness classification already lives in `egress_is_cloud` and stays there (review: code-truth #8 — it's two conflated identities, not three; sizing adjusted).
3. **`egress_is_cloud` gains an explicit `CONN_LOCAL => false` arm** (`summarize/mod.rs`) in the SAME PR as any local-provider work. Without it the default arm (`_ => true`) classifies a local connection as cloud → phantom consent demands, phantom ledger rows, a lying Privacy Receipt. Test: local dispatch writes zero ledger rows; receipt stamps zero cloud calls.
4. **`LocalSummarizerProvider`** (new `summarize/local.rs`, Fully-Local phase): lifts the shared local reasoner into the `SummarizerProvider` seam so Notes/Ask can be served locally. Trait is friendly (no streaming; `complete_json`'s schema-in-prompt default = the `MistralReasoner::structured` technique). Must NOT own weights (§3.3). Built inside `make_provider_resolved` under `CONN_LOCAL`, exempt from redaction wrap + egress sink exactly like loopback Ollama.

### 3.3 Residency & the weight cache (performance-critical)

Today `ReasonerCell.local` is a **single slot** keyed on one GGUF path — alternating light/heavy would thrash multi-GB reloads. mistral.rs has documented drop-leak history (issues #723/#865; 0.8.1 unverified → Spike B). Design:

- **Process-level weight cache in `reason/mistral.rs`:** `path → Arc<Model>` map (cap 2), shared by `ReasonerCell` AND `LocalSummarizerProvider` — one copy of weights per model, ever.
- **Cap-2 semantics** (review: perf #9, code-truth #10): requesting a third model while two are pinned **refuses with a nudge** ("switching models needs an app restart" / "free a slot") — NEVER a silent evict, because evict = drop = the leak-risk operation. If Spike B proves 0.8.1 drops clean, this relaxes to LRU-evict + the idle-drop policy below.
- **Residency policy:**
  - **light:** loaded at **recording start** (pre-warm on record-screen mount). Baseline policy: never dropped. **If Spike B passes:** drop after N hours outside any recording (re-load at record start is 1–2 s) — "never drop" on a tray-resident app is +1.1–3.6 GB of dirty anonymous memory forever, and macOS will compress/swap idle weights anyway, so permanent residency buys Activity-Monitor pain without a real latency guarantee (review: perf #9).
  - **heavy:** loaded on first post-call/Ask use. **Never loaded while a recording is active — on ALL tiers (16 GB included), pending Spike A's long-context RSS measurement** (review: perf #3). Bielik-11B as heavy → post-call only on 16 GB, comfortable at 24 GB+.
  - **Escape hatch (named fallback):** if 0.8.1 still leaks on drop, the long-term answer is an inference **sidecar process** (unload = process exit, immune to in-process leaks). One line here so the option is on the record.
- **RAM guard — includes KV, runs at recording start too** (review: perf #3, product #12): the registry's `min_ram_gb` is per-model-alone and lies for co-residency during a call. Guard = Σ(resident weights) + Σ(`kv_bytes_per_token × max_app_context` per resident model) + fixed call-overhead budget (~4 GB: OS pressure + Zoom/browser + webview + whisper + embedder). Estimates: Qwen3-1.7B ≈ 112 KiB/tok (negligible at ~500-tok Stage-1 prompts); Qwen3-4B ≈ 144 KiB/tok ⇒ an 8–16k-token Notes pass allocates **1.2–2.3 GiB KV** — hence a **heavy in-app context cap of 8k tokens** (excerpt above it; `EXTRACT_EXCERPT_CHARS` precedent) and the heavy-never-during-recording rule. Guard re-evaluates at every recording start (the pre-warm moment), not only at enablement — mid-session RAM changes (user opened Zoom + 40 tabs) are caught by the next recording's check plus the backpressure sensor.
- **Recording-active gate** (review: perf #4 — the biggest real contender is not Stage-1, it's post-call heavy work overlapping the NEXT recording): a single `AtomicBool` recording-active flag in `AppState`; heavy inference, batch facts extraction, and embedder reindex **defer while any recording is live**. This converts "ASR = priority tenant" from slogan to mechanism.
- **Warm-up on every cold launch** (review: perf #8): release builds ship WITHOUT precompiled metallibs (`MISTRALRS_METAL_PRECOMPILE=0` is a build-machine hatch; end users compile shaders at first kernel dispatch, and macOS's shader cache invalidates on app/OS updates). One backgrounded 1-token generation on app launch while Brain Live is enabled — cheap when the cache held, load-bearing when it didn't. First-ever compile can plausibly be 10–60 s (measure in Spike A's cold-start leg); it must never land mid-meeting.

### 3.4 The engine-class handle: `light()` / `heavy()`

Realtime Reactions and facts do NOT ride `Role::Live` (that's the `@brain` assistant — review: code-truth #1). New internal accessor on `ReasonerCell`:

- `light()` — **Brain Live ON:** resolve the posture's light model → loaded instance, or **`StubReasoner` + a degraded-state event** when absent/unloadable (extraction yields empty candidates — provably safe; the FE nudges "Brain Live needs its model re-downloaded"). **NEVER the cloud fallback** (review: code-truth #3, product #1 — the v0.1 "else role-resolved reasoner" order violated P1 in the privacy-critical direction). **Brain Live OFF:** the accessor is not consulted; flows behave as today.
- `heavy()` — Fully-Local Notes/Ask resolution helper via `LocalSummarizerProvider`; same local-or-fail semantics (P1 note-failure UX, §2.1).
- **Facts re-routing is COMPLETE, not partial** (review: code-truth #4, product #3): with Brain Live on, ALL THREE cloud-reasoner-riding flows move to `light()`: `persist_facts_for_meeting` (meeting facts), `persist_user_facts_for_meeting` (user-memory facts), and the **`orchestrate_context` Notes pre-analysis** — decision: pre-analysis moves to `light()` too (it's retrieval-planning, a light-class task; if Spike A shows quality collapse, it falls back to *skip* — the pipeline already treats pre-analysis as best-effort — never silently to cloud). The matrix egress row and card copy reflect exactly this.
- The deterministic `reconcile_facts` core is untouched everywhere.

---

## 4. Realtime Reactions — pipeline & performance budget

### 4.1 The pipeline (cascade — cheap gates before expensive work)

```
whisper.cpp live tick (~3 s cadence, mic-only; EVENT_LIVE_CAPTION)
  └► live_transcript rolling buffer (bounded 16k, merge_live_caption dedup)   [exists]
       └► STAGE 0 — deterministic gate (per tick, no LLM, ~free):
           proactive.rs-style lookup over the caption delta:
           known entity mentioned? open fact/commitment on it?               [exists — reuse]
             ├─ no hit → nothing. (Most of small talk. Zero cost.)
             ├─ hit resolvable WITHOUT new extraction
             │    (entity → open commitment / current fact) → Recall card    [ships today]
             └─ hit + candidate utterance for contradiction/context check
                  └► STAGE 1 — light LLM extraction (throttled, worker thread):
                      windowed extract_fact_candidates variant on the
                      ~2-3 sentence delta (schema-in-prompt OR JsonSchema
                      constraint per Spike C; max_tokens ≈ 128)              [Phase 2]
                        └► STAGE 2 — reconcile_facts, ops NOT applied (pure fn —
                            "dry-run" needs no flag, application is the caller's
                            separate apply_fact_ops call; review: code-truth #9).
                            Existing facts read via the GATED list_facts_visible,
                            NOT the un-gated lifecycle read.
                            Invalidate → contradiction card w/ the OLD fact as
                            extractive citation (meeting_id → [[wikilink]], date,
                            speaker); NoOp/Add → silence
                              └► WhisperCard event → FE rail (signals store)
```

**Threading (review: code-truth #11, perf #7):** `LocalReasoner` is synchronous/blocking and the live loop is a dedicated 3 s-tick thread. Stage-1 runs on its **own worker thread** (the voice-command-dispatch "OFF the tick" precedent), never inline in the tick — a 2–10 s extraction inline would stall captions entirely.

**Why this is trustworthy by construction:** the LLM never *judges* — it only normalizes a sentence into a triple; the verdict is the deterministic reconcile against a real DB row, and the citation is extractive (the existing fact), never generated. Bad extraction ⇒ a missed card (safe), never a hallucinated accusation.

### 4.2 Trigger, throttle & calibration discipline

- Stage-1: **max 1 in flight; min 20 s between calls; only on Stage-0 hits; skip-if-busy** (drop the trigger, the meeting moves on). In-flight calls are non-preemptible (§3.1) — the throttle prevents the next call, never cancels the current one.
- Card emission: cooldown + score threshold in the `proactive.rs` mold (≤1 card/120 s baseline); Dismiss/Pin feedback tunes a per-person local threshold.
- **Contradiction cards: shadow mode first, user-local calibration** (review: product #8 — "measured precision" without telemetry would mean the developer's own meetings, which cannot honestly gate default-on for everyone). Phase 2 ships contradiction detection running **silently**: would-have-fired events are journaled as DB **counters only** (counts, no content — no-PII discipline). The sub-toggle is offered per-user once *their own* shadow numbers clear a stated bar ("in your last 10 meetings the brain would have flagged N contradictions; want to see them live?"). Initial thresholds come from the developer's own meetings — stated plainly, per the honesty bar. Calibration is thus local, per-user, zero-egress.
- Framing rule: neutral copy ("earlier he said X — [[source]]"), never accusatory. A legitimate change of mind is not deception.

### 4.3 Latency & compute budget

| Stage | Cost | Budget |
|---|---|---|
| Stage 0 (DB lookup) | SQLite point reads over caption delta | < 10 ms, every tick |
| Stage 1 (light LLM) | prompt ≈ 400–600 tok (system+schema; prefix-**cached if the HIT verifies** — see below) + **max_tokens 128** | target ≤ 5 s p50 on M2-base-class under live whisper load; ≤ 10 s p95 on M1 base; **over-budget ⇒ skip, don't queue** |
| Stage 1, cache-MISS row | + full prefill ≈ +0.7–2 s (M2-base) / +1–3 s (M1-base) | a stated, survivable regime — not a surprise (review: perf #6) |
| Stage 2 (reconcile) | pure fn, hashmap | µs |
| End-to-end card | utterance end → card visible | "just now" feel: ≤ ~8 s. Ambient, not blocking — this is NOT sub-second UX and must not pretend to be. |

**Anchor honesty (review: perf #1):** the numbers above derive from llama.cpp Apple-Silicon anchors scaled 7B→1.7B — but the runtime is **mistralrs**, whose Metal-kernel efficiency vs llama.cpp is unmeasured. Spike A therefore measures an **engine factor** (same GGUF through llama-bench AND `MistralReasoner`, same Mac) and the budget is re-derived as `anchor × engine factor × contention factor`. **Pre-committed contingency:** if p50 > 5 s on M2-base after tuning, Phase 2 ships recall/context-only and contradiction cards wait for a faster path (smaller model / AFM / engine upgrade) — the budget does not get renegotiated post-hoc.

**Contention mitigations:**
- Light model (1.7B) + `max_tokens` 128 (decode dominates) + prefix caching (default-ON in 0.8.1; **HIT across separate requests is unverified — Spike A tests it explicitly**: two consecutive calls, pass = call-2 prefill < 30 % of call-1). A HIT is not free — cached prefixes pin KV (counted in the RAM guard).
- **D1 (new work, not existing design — review: perf #5, code-truth #7):** today the live tick loads the **user-configured** whisper model (`start_recording` → `resolve_model_path(cfg.whisper_model_path, cfg.model_size)`) — `small` is merely the default, and a `large-v3` user's live tick alone can eat multi-second GPU slices every 3 s. D1: while Realtime Reactions is enabled, pin the live tick to `small` **if `ggml-small` is on disk**, else keep the configured model + warn ("reactions may degrade live captions with this model") — pinning to an absent file would kill the live loop entirely. Post-call accurate pass unchanged. Change-map row added.
- **Backpressure sensor (specified mechanism — review: perf #7):** the live loop times its own `transcriber.transcribe` call and publishes an EWMA to `AppState` (AtomicU64). Baseline = the first ~5 ticks of each recording, before any Stage-1 fires (the 20 s min-interval guarantees that window). If tick duration degrades > 1.5× baseline while an extraction is in flight → back off Stage-1 (double the min-interval; restore on recovery). ASR is the priority tenant; reactions yield. This sensor also indirectly catches mid-session RAM pressure (§3.3).
- The recording-active gate (§3.3) keeps heavy/batch work off the GPU during any recording — the *actual* biggest contention source (post-call heavy Notes overlapping the next meeting), which Stage-1 throttling alone cannot address.
- Battery/thermals: whisper alone is ~35–50 % GPU duty during a meeting; Stage-1 bursts take combined duty to ~55–70 % (estimate). On fanless Airs the plausible real-world behavior is throttle → slower ticks → the sensor silences Stage-1 — a **designed degradation, stated in the enablement card** ("on fanless Macs under heavy thermal load, reactions may pause"), not a discovered one (review: perf #10). §7's battery test is an on-battery A/B with throttle-onset measurement.

### 4.4 Data & privacy invariants (lock-model compliance)

- WhisperCards are **ephemeral events** (proactive-hint mold): RAM → FE signal store; **not persisted** in Phase 1–2. If persisted later: meeting-anchored rows purged-on-seal like `assistant_interactions`.
- **Emission gating:** Stage 0–2 read ONLY through visibility-gated readers (`visibility_clause` family; Stage 2 uses `list_facts_visible`, never the un-gated lifecycle read). A sealed-not-unlocked folder's facts/commitments can never surface in a card. Additionally the **current meeting's own visibility** is checked per the `gated_live_context` house pattern (`meeting_is_visible`) — covers a recording moved into a sealed folder mid-call (review: product #10).
- **FE rail purge on lock transitions (the convertFileSrc-analogue trap — review: product #2, CRITICAL):** content that already crossed to the FE outlives the backend gate unless the FE purges it. Invariants: (a) the rail subscribes to the same screen-share-start signal that clears today's single hint card (`screen-share.service` → clear) and **clears/masks ALL cards on share start**; (b) any lock transition (`lock_folder`/`relock_all`) purges cards whose source meeting is no longer visible; (c) **pinned cards re-verify source visibility before every render**. Also fix the pre-existing gap in the same PR: manual "Lock all" does not clear today's hint card (only screen-share does).
- Zero egress by construction: every stage on-device; no redaction needed because nothing leaves. The **egress ledger** is the user-visible proof surface for these layers.
- No PII in logs: stages log counts/ids/latencies only (shadow-mode journal: counters only).
- Voiceprint attribution on cards uses the existing opt-in voiceprint layer when present; absent → meeting + date only. No new biometric surface.

---

## 5. Download, onboarding & the default question

- **Downloader hardening (prerequisite for any "default-offered" story):** `download_brain_model` gains HTTP Range **resume** + pinned **SHA-256** per registry entry (+ the existing progress events). A 1–2.5 GB one-shot download with no resume dies on hotel Wi-Fi and takes the flagship's reputation with it. A failed SHA check ⇒ delete + offer re-download (never load an unverified file — feeds P1).
- **Onboarding:** the posture step replaces/extends the current provider step. Hybrid preselects on ≥16 GB with the light download default-checked (skippable — skipping ⇒ Cloud posture + Recall-card floor + later in-context nudges). **Stated totals** (review: product #11): Hybrid first-run download ≈ whisper ~0.5 GB + light ~1.1 GB ≈ **1.6 GB** — the step copy says so. **Record never blocks on brain download/warm-up** (deterministic floor + whisper suffice for first value); the download runs backgrounded through the remaining steps and ends with the warm-up inference, whose duration shows on the progress UI, not silence.
- **No sentinel default-flip.** The posture becomes Hybrid **only on successful download + SHA verify + warm-up** (fresh installs); existing users get a nudge card, never a silent flip.
- Distribution: model files stay on HF (murmur-io mirror later — also reduces download-metadata exposure); the DMG never bundles weights (GitHub 2 GiB release-asset cap; re-notarizing gigabytes per release for unchanged weights is waste).

---

## 6. Change-map (seam → change → size → phase)

| # | Seam | Change | Size | Phase |
|---|---|---|---|---|
| 1 | `reason.rs` registry | retire `qwen2.5-3b` **+ installed-base migration + test-fixture rework**; add qwen3-1.7b/4b-2507 (+`class`, `sha256`); `enable_thinking=false` | S–M | **P0 (now)** |
| 2 | `reason.rs` downloader | Range resume + SHA-256 verify | S | P1 |
| 3 | `reason/mistral.rs` | process-level `path → Arc<Model>` cache (cap 2, refuse-don't-evict); **`max_tokens`/sampler plumbing through `LocalReasoner`** | M | P1 |
| 4 | `reason.rs` `ReasonerCell` | slot → keyed map over the shared cache; `light()`/`heavy()` accessors with P1-safe local-or-stub semantics | S–M | P1 |
| 5 | config + FE | **derived** posture display + preset writer + Brain Live enablement card + combined-RAM guard (incl. KV line, re-eval at recording start) | M | P1 |
| 6 | onboarding | posture step + backgrounded download + SHA + warm-up; stated totals; Record-never-blocks | M | P1 |
| 7 | live pipeline + FE | **the Reactions rail**: new FE surface consuming Recall cards (Stage-0, no LLM — flag-only, model-independent) + `EVENT_WHISPER_CARD`; **rail purge on share-start + lock transitions (incl. the pre-existing "Lock all" gap); pinned-card re-verify** | M | **P1** |
| 8 | `transcribe/live.rs` + `AppState` | D1 live-tick pin (small-if-present else warn); tick-duration EWMA sensor; recording-active `AtomicBool` gate | S–M | P1 |
| 9 | live pipeline | Stage-1 worker thread (windowed `extract_fact_candidates` variant) + Stage-2 gated dry-run reconcile + contradiction cards in **shadow mode** → per-user sub-toggle | M | P2 |
| 10 | facts + user-memory + pre-analysis | ALL THREE `Role::Notes`-reasoner flows → `light()` when Brain Live on (`persist_facts_for_meeting`, `persist_user_facts_for_meeting`, `orchestrate_context`) | S–M | P2 |
| 11 | `summarize/roles.rs` | predicate split (`builds_no_provider` / `is_agentic_eligible`) + identity-matrix tests (incl. posture reverse-mapping) | S–M | P3 |
| 12 | `summarize/mod.rs` + `summarize/local.rs` | `CONN_LOCAL` provider arm + `LocalSummarizerProvider` + `egress_is_cloud => false` arm (same PR) + heavy 8k context cap | M | P3 |
| 13 | FE settings | Fully-Local posture unlocks local Notes/Ask (Notes-row unhide) + Error-note recovery UX (§2.1) | S–M | P3 (gated on #14) |
| 14 | eval | **BUILD a notes-generation bake-off harness (the existing `eval/bakeoff.rs` is retrieval-only — it cannot score notes; review: code-truth #2), then run** local vs cloud on ≥20 real meetings incl. PL | M | gate for P3 |
| 15 | fine-tune `murmur-extract` | LoRA on 1.7B, Apache/MIT teachers, merged GGUF | L | P4 (gated on P2 baseline) |
| — | reviews | lock-security (rail purge, gated reads, egress classifier/receipt) + adversarial (RED-before-GREEN per phase) | — | every phase |

**Gating summary:** the Reactions **rail** (P1) is flag-only — it works with zero downloads (Recall cards). `light_model_present()` gates **Stage-1/2 only** (P2) — a no-LLM phase gated on model presence would be incoherent (review: code-truth #5). The new config key (`brain_live` + `realtime_reactions_v2` sub-toggle or similar) is backend-gated (`flag && presence` at the decision point — the `should_auto_index` two-condition house pattern); FE disabling alone is not enforcement. The existing `realtime_reactions` flag keeps its wake-word-dispatch meaning untouched.

---

## 7. Verification plan (the honesty bar)

**No forward pass has ever been verified on this project** (`reason/mistral.rs` header states it). Headless CI proves wiring only. Real-Mac gates, in order:

1. **Spike A (P0/P1 gate, one day):** Qwen3-1.7B download → cold-start leg (measure first-ever shader-compile + load, then app-relaunch → first-inference — the update case) → **engine-factor leg** (same GGUF via llama-bench vs `MistralReasoner`; record decode-t/s ratio) → **prefix-HIT leg** (two consecutive structured calls; pass = call-2 prefill < 30 % of call-1) → the live-load leg: run the windowed extraction over 20 PL + 20 EN transcript windows **while a live recording ticks with D1's pinned `small`**. Measure: JSON validity %, triple plausibility, per-call wall-clock (p50/p95), tick-EWMA degradation, RSS with a Meet tab open (incl. one long-context heavy call for the KV number), `powermetrics`. Pass: ≥90 % valid JSON, ≤5 s p50 / ≤10 s p95, tick degradation < 1.5×, no red memory pressure.
2. **Spike B (residency, 1 h):** 10× load/drop of a 2 GB GGUF on mistralrs 0.8.1 watching RSS — **including drop-while-request-in-flight and drop-from-async-context** (the `Runtime` ownership + `local_cached` swap path can drop from arbitrary call sites). Decides: idle-drop allowed? cap-2 evict allowed? Else never-drop + sidecar fallback stays.
3. **Spike C (1 h, optional win):** retry `Constraint::JsonSchema` grammar-constrained decode with the tiny 3-field facts schema on 1.7B (the historical overflow was Bielik-11B + a large schema). If it works, Stage-1 gets deterministic JSON for free.
4. **Phase-1 acceptance:** live smoke — Recall card appears in the rail on a real meeting; **rail purges on screen-share start and on Lock-all** (RED: card visible → share starts → rail empty); sealed-folder content provably never surfaces (lock-security review); ledger shows zero egress for the layer.
5. **Phase-2 acceptance (RED-before-GREEN):** a scripted transcript with a planted contradiction produces the card with the correct extractive citation; a paraphrase-only change produces NO card (false-positive guard); shadow-mode counters accumulate correctly; the sub-toggle only offers after the stated shadow bar.
6. **P3 gate:** the NEW notes bake-off harness (built in #14), blind side-by-side (Qwen3-4B vs Bielik-11B vs cloud default) on ≥20 real meetings incl. Polish. Local Notes ships as a choice only if within a stated preference margin.
7. **Battery/thermal A/B:** 1 h recorded call on battery (not AC) on a fanless Air, reactions ON vs OFF — battery-% delta, sustained package power, throttle onset, tick-latency log. The "reactions may pause" degradation is verified as designed behavior.

---

## 8. Risks & mitigations

| Risk | Severity | Mitigation |
|---|---|---|
| Noisy/wrong whisper cards burn trust in the flagship | **highest** | deterministic-verdict design (LLM never judges); Recall/context first; contradiction in shadow mode → per-user opt-in; throttle+cooldown; neutral copy; Dismiss feedback |
| Sealed content outlives the gate in the FE rail | **high (leak-class)** | §4.4 purge invariants (share-start, lock transitions, pinned re-verify) + lock-security review each phase |
| Metal contention starves live captions | high | 1.7B + max_tokens 128 + prefix cache; D1 pin; EWMA backpressure; recording-active gate for heavy/batch; ASR = priority tenant |
| mistralrs drop-leak forces never-drop | medium | refuse-don't-evict cap; Spike B decides; sidecar-process fallback named |
| Latency anchors wrong for mistralrs | medium | engine-factor measurement; pre-committed contingency (recall-only Phase 2) |
| 8 GB Macs | medium | RAM guard (incl. KV) → light-only tier; AFM (ANE, zero Metal contention) as the future 8 GB answer |
| License (qwen2.5-3b) | **now** | P0 retirement + installed-base migration (no silent stub-downgrade) |
| Silent egress via classifier default (`_ => true`) | high | explicit `CONN_LOCAL => false` arm + ledger-empty test in the same PR |
| Posture label lies after hand-edits | medium | posture = derived display state; "Custom" for non-preset combos; reverse-mapping tests |
| Fully-Local note failure reads as content loss | medium | Error status + transcript intact + two explicit recovery actions (§2.1) |
| Polish quality of Qwen3 small models | medium | Bielik alternates per class; notes bake-off gates P3; Spike A measures PL extraction |
| Download friction | low-medium | soft gate + Recall floor; resume+SHA-256; backgrounded onboarding; stated 1.6 GB total; Record never blocks |

---

## 9. Open questions

1. Spike A/B/C outcomes (every performance number is an estimate until then; the §4.3 table is re-derived from Spike A's engine factor).
2. AFM sidecar as light-engine backend (`light_present() || afm_available()`): when to invest in sidecar completion? (It is also the cleanest 8 GB and battery answer.)
3. Card persistence (Phase 3+): post-meeting "what the brain caught" digest? (If yes: meeting-anchored rows, purge-on-seal.)
4. Posture naming in UI (PL/EN): "Cloud / Hybrid / Fully local" vs friendlier labels.
5. Whether Hybrid should also re-route **Ask** to local heavy when present (privacy win vs quality risk — current answer: no, Ask stays with Default AI until the bake-off says otherwise).
6. `murmur-extract-1.7b` training-data pipeline (synthetic PL meeting windows from Bielik-11B/Qwen3-large) — only if Spike A's prompt-only baseline disappoints.
7. The `orchestrate_context` pre-analysis quality on the light model (P2 #10 decision: light-or-skip) — validated in Spike A's PL/EN windows or a small dedicated check.

---

## 10. Review log (v0.1 → v0.2)

Three independent adversarial reviews of v0.1, all **PASS_WITH_FIXES**; every blocking finding incorporated:

- **Code-truth lens:** 12 findings. Blocking: Hybrid preset must not touch `role_live_*` (would lobotomize the shipped `@brain` agentic loop → §2.1/§3.4 engine-class handle); `light()` cloud-fallback violated P1 (→ local-or-stub); facts re-route was ⅓ complete (→ all three flows, §3.4); Phase-1 gating incoherent (→ rail flag-only, presence gates Stage-1/2); `eval/bakeoff.rs` mis-cited (retrieval-only → build a notes harness, #14); qwen2.5-3b retirement needed an installed-base migration (→ §3.1); `brain_backend=Off` maps to Custom/Off, never Cloud.
- **Performance lens:** 10 findings. Blocking: latency anchors are llama.cpp but the runtime is mistralrs (→ engine-factor leg + pre-committed contingency); the `max_tokens` cap had no mechanism (→ P1 plumbing, #3); RAM guard ignored KV (→ KV line + heavy 8k cap + heavy-never-during-recording on all tiers); no arbitration for post-call heavy work overlapping the next recording (→ recording-active gate); D1 was stated as existing design but is new work (→ #8 + fallback rule); prefix-HIT unverified (→ Spike A leg + cache-miss budget row); backpressure sensor was fictional (→ EWMA mechanism specified); warm-up must run every cold launch, not once at download; never-drop is user-hostile if Spike B passes (→ idle-drop middle path + sidecar fallback); battery test → on-battery A/B with stated degradation mode.
- **Product+privacy lens:** 12 findings. Blocking: FE rail purge on relock/screen-share (the convertFileSrc-analogue leak — → §4.4 invariants + the pre-existing Lock-all gap); posture must be derived state, never stored (→ §2.1); Fully-Local note-failure UX specified (→ §2.1); calibration without telemetry → user-local shadow mode (→ §4.2); enablement-card copy made conditional + HF-download nuance + ledger-not-receipt as the proof surface (→ §2.2); Recall cards vs Realtime Reactions named as two tiers so the matrix stops contradicting the floor (→ §2.1/§2.2); current-meeting visibility check (→ §4.4); stated 1.6 GB onboarding total + Record-never-blocks (→ §5).
