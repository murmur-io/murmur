# Brain v2 + Transcription Performance — Master Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **Murmur override:** execution follows this repo's binding agentic workflow (`.claude/rules/agentic-workflow.md`) — Workflow tool, builder ≠ verdict-owner, `adversarial-verifier` owns PASS/FAIL, `lock-security-reviewer` required for every lock/crypto/visibility-touching phase.

**Goal:** Ship Brain v2 (measured retrieval, engineered context, consolidated memory, gated live brain, ClickUp-parity agents) plus the transcription heat/quality fixes, as a sequence of independently-shippable, gate-verified PRs.

**Architecture:** Two coupled workstreams over one tree — WS-B (Brain v2, the 7-PR spec in `docs/research/2026-07-09-brain-v2-architecture-spec.md`) and WS-T (transcription, T0–T4 in `docs/research/2026-07-09-transcription-performance.md`). Everything is additive (new tables `IF NOT EXISTS`, `add_column_if_missing`, config keys `#[serde(default)]`); the substrate (SQLite canonical, lock gating, provider seam + redaction + ledger) is untouched. Measurement gates (eval fixture, powermetrics baseline) precede the changes they judge.

**Tech Stack:** Rust (Tauri 2.11, `meetnotes_lib`), whisper-rs 0.16 (Metal), mistralrs GGUF, candle e5, sherpa-onnx 1.13.3, SQLCipher, Angular 22 zoneless (minor FE surface only).

## Global Constraints (binding, from CLAUDE.md + rules)

- Inner loop = `cargo test --lib` from `src-tauri/` — NEVER `cargo clippy --all-targets`; `scripts/ci.sh` once at the end of a phase.
- Errors: `AppError`/`Result<T>` only; no `unwrap()`/`expect()` in non-test code; locked-content refusals are `AppError::Locked`.
- New Tauri command ⇒ `commands.rs` AND `lib.rs generate_handler![]` in the same change; FE gets ONE typed `IpcService` method + type in `core/models.ts`.
- Migrations ADDITIVE only; every new content read gated (`meeting_is_unlocked` / `visibility_clause`); every new seal verify-before-destroy; new index/cache rows purge-on-seal.
- No PII in logs (IDs/stages/counts/durations only). No new npm packages or crates without explicit user approval (sherpa-onnx, objc2-foundation, libc are already in-graph).
- Commits authored `QueaT <kgm004a@gmail.com>`, no Claude trailers; merge to `murmur` via PR only; `gh` account JakubGawr; `com.meetnotes.app` immutable.
- RED before GREEN for every bug fix; FE runtime checks via Playwright against `:1420` with mocked `window.__TAURI_INTERNALS__.invoke`.
- Honesty bar: watts/thermal/caption-quality/Touch-ID/boundary-timing claims need a real Mac (some need sudo `powermetrics` or a real meeting); a green headless gate is NOT proof for them — say so in every DoD.

## Current state (analyzed 2026-07-09 ~23:30)

- **PR-0 payload already in the working tree, UNCOMMITTED** (a concurrent agent is finishing it): spec-P0 complete — P0.1 `title_looks_like_person_name` fallback in `summarize/redact.rs` (+ RED test `person_name_title_is_dropped_when_no_ner_model_present`), P0.2 stub floor guard in `voice_action.rs` (+ `stub_floor_hints_settings_and_never_echoes_stub`), P0.3 `GenOptions::live_answer()/ask_answer()` presets + `reason_with_opts` + `GENERATION_TIMEOUT=30s` `run_with_timeout` (leaks worker; mistralrs uncancellable) in `reason.rs`/`reason/mistral.rs`, `run_agentic_loop(…, opts)` in `agent.rs`, `in_flight_turns` dedup (`try_begin_turn`/`TurnGuard`) + `user_turn_in_progress` priority in `state.rs`/`transcribe/live.rs`, reactions `should_defer_scan` in `brain_reactions.rs`, `ask_vault_loop` wired in `commands.rs`. ~857 insertions, 10 files, tests included.
- **Not part of PR-0** (leave out of the commit): `.claude/skills/deploy-murmur-server/SKILL.md` (unrelated concurrent edit), `.agents/`, `.codex/`, `whisper-01.png`, `docs/dreams/*`, `.claude/learnings/landing-api-deploy.md`.
- **Research docs to commit** (docs-only, can ride PR-0 or a separate docs commit): `docs/research/2026-07-09-brain-v2-architecture.md`, `…-spec.md`, `…-transcription-performance.md`, plus this plan.

## Scope check → phase plans

This is a program, not one feature. Per plan discipline: **each phase below is a PR-sized sub-plan that ships working, gate-verified software on its own.** Phases whose exact content depends on a measurement gate (PR-3 depends on the PR-2 baseline; T2's default flip depends on the T0 harness; T4/PR-6 substrate depends on the T3 spike) get their fully bite-sized plan written **just-in-time at execution**, from the spec section cited in the phase header — writing line-level code for them now would fabricate details the gates exist to decide. Phases PR-0, T0, T1 are next up and are specified to near-executable detail here.

## Dependency graph

```
PR-0 (P0 hotfixes, land other agent's work)            ← FIRST, unblocks live.rs/agent.rs
  ├─→ WS-T: T0 measure → T1 free wins → T2 batch default (T2 gated on T0 harness + user OK)
  │            └─→ T3 parakeet spike (independent files; anytime after T0)
  ├─→ PR-2 eval bootstrap (USER labels 20 queries — start early, long pole)
  │        └─→ PR-3 L1 retrieval  ─┐
  ├─→ PR-4 L2 memory (∥ PR-3)      ├─→ PR-5 L3 orchestration (after PR-3 eval + PR-0 seams)
  └────────────────────────────────┘         └─→ PR-6 L4 live brain (after T1; substrate ← T3 verdict;
                                                     T4 LocalAgreement folds in here if adopted)
                                              └─→ PR-7 L5 agents (briefs ∥ MCP client ∥ verify pass)
```

**File-conflict serialization** (the real constraint; ⊥ = safely parallel):
`transcribe/live.rs`: PR-0 → T1 → PR-6/T4 (strictly serial). `agent.rs`/`reason*.rs`: PR-0 → PR-5. `db.rs`/`embed.rs`: PR-3 → PR-4 only if both touch `user_facts` FTS — PR-3 ⊥ PR-4 otherwise. WS-T (T0/T2/T3: `whisper.rs`, `model.rs`, `scripts/`, `eval/`) ⊥ PR-2/3/4. PR-7's three items mutually ⊥.

---

### Phase PR-0: Land the P0 hotfixes (other agent's work) — size S, TODAY

**Files:** commit exactly: `src-tauri/src/{agent.rs, brain_reactions.rs, commands.rs, reason.rs, reason/mistral.rs, state.rs, summarize/redact.rs, transcribe/live.rs, voice_action.rs}` + the 3 research docs + this plan. **Exclude** the not-part-of-PR-0 list above.

**Interfaces produced (later phases rely on these):**
- `crate::reason::GenOptions::{live_answer(), ask_answer()}`; `LocalReasoner::reason_with(system, user, opts)`.
- `crate::agent::run_agentic_loop(reasoner, system, user, executor, max_steps, sink, opts: GenOptions)`.
- `AppState.in_flight_turns: Mutex<HashMap<String,u32>>` + `voice_action::{try_begin_turn, end_turn}`; `AppState.user_turn_in_progress: AtomicBool` (T1's tick-defer and PR-6's gatekeeper read this).
- `summarize::redact::title_looks_like_person_name(&str) -> bool`.

**Steps:**
- [ ] Wait for the concurrent agent to signal done (do NOT edit the 10 files meanwhile; no `cargo` runs while it may hold the target-dir lock).
- [ ] Run gates: `( cd src-tauri && cargo test --lib )` — expect green incl. the new `person_name_title_is_dropped_when_no_ner_model_present`, `stub_floor_hints_settings_and_never_echoes_stub`, `run_with_timeout_*`, `turn_dedup_*`, `scan_defers_only_while_a_user_turn_is_in_flight` tests. Rust-only change ⇒ `ng lint`/`ng build` not required, but run `npx ng build` once anyway (cheap insurance before PR).
- [ ] Dispatch **adversarial-verifier** (owns the verdict): hunt (a) a redaction false-positive dropping a real meeting-title wikilink, (b) the timeout path leaking Metal state mid-generation on repeated timeouts, (c) turn-dedup dropping a LEGITIMATE second question after a crash left the counter high (is `end_turn` reached on every exit path? `TurnGuard` must be drop-based), (d) reactions permanently starved if `user_turn_in_progress` wedges true.
- [ ] Dispatch **lock-security-reviewer** (REQUIRED — `redact.rs` is the egress firewall): verify the fallback only ever DROPS titles (never adds egress), and the no-NER gating can't be bypassed.
- [ ] Stage exactly the files above (`git add <explicit paths>` — never `-A`), QueaT commit, `gh pr create` → merge to `murmur`.

### Phase T0: Measurement baseline — size S, needs the user's Mac + sudo

**Files:** Create `scripts/measure-live-power.sh`; Modify `src-tauri/src/transcribe/live.rs` (per-tick decode duration log, non-PII) — **after PR-0 merges**.

**Steps:**
- [ ] Add to the tick loop: `tracing::info!(target: "live_perf", decode_ms, window_s = WINDOW_SECS, model = %model_label)` (counts/durations only; model size label, never a path with user content).
- [ ] `scripts/measure-live-power.sh`: wraps `sudo powermetrics -i 1000 --samplers cpu_power,gpu_power,ane_power,thermal --show-process-energy -o "$OUT"` around a timed recording session; README block documents the protocol (10-min scripted meeting, small vs large-v3 live, same audio).
- [ ] Add `#[ignore]` A/B harness test in `src-tauri/src/transcribe/whisper.rs` tests: env-driven (`MURMUR_ASR_AB_WAV`, `MURMUR_ASR_AB_MODEL_A/B`) — decode one fixed PL/EN WAV through both configs, print wall-clock + transcript diff paths. (Same env-driven pattern as `run_bakeoff_over_real_db_from_env`.)
- [ ] **USER-ASSISTED:** run the protocol on the real Mac (sudo prompt); commit numbers as `eval/results/live-power-baseline.md` (date, chip, commit sha, per-config watts/duty). This artifact is the bar every T1/T2/T3 change must beat.

### Phase T1: Live-loop free wins — size M, five independent commits, each RED→GREEN

**Files:** Modify `src-tauri/src/transcribe/{whisper.rs, live.rs, vad.rs}`, `src-tauri/src/commands.rs` (live-model pin), `src-tauri/src/audio/listener.rs`, `src-tauri/src/settings/config.rs` (new keys, all `#[serde(default)]`); Create `src-tauri/src/thermal.rs`.

**Interfaces produced:** `thermal::ThermalGovernor { fn effective_tick(&self) -> Duration; fn captions_suspended(&self) -> bool; fn reactions_paused(&self) -> bool }` (PR-6's boundary/bullets cadence reads this); config keys `live_vad_gate` (default true), `live_model_pin` (default "small", "" = configured model escape hatch).

- [ ] **T1.1 flash_attn(true)** — in `Transcriber::load`: `params.flash_attn(true)` on `WhisperContextParameters`; test = existing profile-wiring unit test extended to assert the param (wiring-level; quality A/B rides the T0 harness). One commit.
- [ ] **T1.2 audio_ctx=832, Fast profile ONLY** — `build_params`: Fast arm sets `set_audio_ctx(832)` (14 s window: `(14/30)*1500+128 ≈ 828 → 832`, multiple of 64, >768 floor); Accurate arm untouched (RED: a test asserting Accurate never sets audio_ctx). One commit.
- [ ] **T1.3 unconditional live pin** — `commands.rs` `live_model` selection: drop the `brain_live` condition; pin to `small` when present, honor `live_model_pin` config; fall back to configured model when small absent. Also `audio/listener.rs`: wake listener resolves the SMALLEST downloaded model (tiny → base → small), never medium/large (wake matching needs rough text only). RED: selection unit tests for both. One commit.
- [ ] **T1.4 Silero tick gate** — new pure fn in `live.rs`: `speech_in_delta(vad: &mut VadSegmenter, delta_16k: &[f32], hangover: &mut u8) -> bool` — run Silero (CPU-ONLY — `vad.rs` documents the second-Metal-context `ggml_abort`) on the NEW 3 s delta only; on speech set `hangover = 2`, decode while `hangover > 0`; **bypass while `step_manual_capture` is armed** (its stop-budget is tick-counted) and while a wake-suppression window is active. Gate behind `live_vad_gate`. RED: gate-decision unit tests (speech/silence/hangover/bypass matrix) with a stub VAD trait. One commit.
- [ ] **T1.5 thermal governor + QoS + turn-defer** — `thermal.rs`: `NSProcessInfo.processInfo.thermalState` via the in-graph `objc2-foundation` (additive `NSProcessInfo` feature; rule-7: guarded, degrade to `nominal` on any FFI doubt — never panic); map `fair`→tick 6 s, `serious`→tick 9 s + `reactions_paused`, `critical`→`captions_suspended` (recording + batch NEVER touched). Tag caption/reactions threads via `libc::pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0)`. Tick-defer: skip one decode when `user_turn_in_progress` is set and the resolved live backend is local-GGUF. RED: governor mapping table test + defer test (mirror `scan_defers_only_while_a_user_turn_is_in_flight`). One commit.
- [ ] Gates: `cargo test --lib` + adversarial-verifier hunting: caption gaps at utterance tails (hangover too short), wake-word misses during gated silence (wake needs the DECODED text — verify the gate can't starve wake detection while the user is speaking), governor flapping (hysteresis: only degrade-fast/recover-slow), manual-capture starvation.
- [ ] **USER-ASSISTED:** re-run the T0 powermetrics protocol; commit `eval/results/live-power-after-T1.md`; expect ≥5× duty cut vs baseline. Honest DoD: caption quality under gating needs a real bilingual meeting.

### Phase T2: Batch quality default — size S–M, gated on T0 harness + explicit user OK

**Files:** Modify `src-tauri/src/transcribe/model.rs` (`model_filename` quant variants + turbo default), `src-tauri/src/settings/config.rs`, onboarding/settings FE copy (model picker labels).
- [ ] Extend `model_filename` to accept `large-v3-turbo-q8_0` etc. (HF mirror already hosts them); keep every current size selectable.
- [ ] Run the T0 A/B harness: `small` vs `large-v3-turbo-q8_0` on the fixed PL/EN WAV (wall, RAM, transcript diff). Present diff to user.
- [ ] **DECISION GATE (user):** flip default `model_size` for pl/auto to turbo-q8_0 (RAM-guard-checked at load; `small` fallback below the RAM floor). Ship only after explicit OK — default flips are user-visible.

### Phase T3: Parakeet live spike — size S (1 day), anytime after T0; decision input for PR-6/T4

**Files:** Create `src-tauri/src/transcribe/parakeet_spike.rs` (`#[ignore]` tests only, no shipping path yet).
- [ ] Download `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8`; drive `sherpa_onnx::OfflineRecognizer` (`model_type = "nemo_transducer"` — crate 1.13.3 documents this; FIRST check: does 1.13.3's bundled C lib include the #2605/#2606 int8 missing-words fix — a truncated transcript on the test WAV is the symptom).
- [ ] Same WAV through parakeet-int8 vs current live path: wall, powermetrics, RSS, transcript diff — **specifically PL↔EN code-switching (auto-LID, no forced pl) and silence behavior**.
- [ ] **DECISION GATE:** adopt parakeet for the live path in PR-6 (new `LiveAsr` seam) only if PL quality ≥ today's small-greedy AND energy materially better. Attribution line (CC-BY-4.0) added to About when adopted. Escalation if hot: FluidAudio ANE sidecar (new dep — user approval).

### Phase PR-2: Eval gate bootstrap — size S + USER labeling (spec §L1.6) — START EARLY

- [ ] Commit `eval/fixtures/rag-bakeoff-sample.json` (20 queries, 4 categories: entity-anchored / paraphrase / cross-lingual PL↔EN / temporal) with empty `expected_meeting_ids`; extend the existing `run_bakeoff_over_real_db_from_env` with `RetrievalMode::Reranked` slot + `MURMUR_BAKEOFF_OUT` markdown artifact writer (`format_report_markdown`: date, sha, consts, recall@5 / nDCG@5 / MRR per mode).
- [ ] **USER-REQUIRED:** label the 20 queries against the real dev vault (WAL-copy + dev DEK protocol per `docs/RAG-BAKEOFF.md`) — the one step no agent can do.
- [ ] Run baseline on the dev vault; commit `eval/results/rag-bakeoff-latest.md`. **Merge rule from here on:** any retrieval-touching PR re-runs it; hybrid recall@5 ≥ baseline (manual, adversarial-verifier-checked; CI can't run it — needs model + real DB).

### Phase PR-3: L1 retrieval — size M (spec §L1.1–L1.5; detailed plan written JIT after the PR-2 baseline exists)

Content locked by spec: topic-segment indexing (`embed::segment_topics`, deterministic lull/speaker-run/Jaccard boundaries; `topic_chunks` + vec + external-content FTS + triggers; **purge-on-seal via `purge_chunks_tx`**; idempotent startup backfill) → deterministic contextual augmentation (`augment_chunk_text`: `title | date | attendees≤5 | facts≤8` header on BOTH FTS and embedding legs, `visibility_clause`-gated readers, sealed ⇒ empty header) → score fusion (`score_fuse` min-max 0.4/0.4/0.2, RRF fallback) → `rerank.rs` trait (PromptedReranker on resident Qwen-1.7B, 3 s deadline, degrade-to-input-order, Ask-only) → `summarize/temporal.rs` PL+EN date parser threading `date_filter` into all three `search_hybrid_visible` legs. Gates: eval artifact updated, recall@5 ≥ baseline; **lock-security-reviewer** (new tables/purge paths); RED tests per spec (`date-filtered search excludes out-of-window meeting`, seal-purge round-trip).

### Phase PR-4: L2 memory — size S–M (spec §L2; ∥ PR-3)

Relevance-filtered brief (`fts_user_facts` + gated `search_user_facts_visible(query, k=8, unlocked)`, behavior-preserving fallback) → hourly consolidation job (`memory.rs`: recency 0.995^h ×0.4 + importance ×0.4 + relevance ×0.2 into `memory_scores` FK-CASCADE; light-reasoner reflection → `memory_rollups` → `.md` export to `<vault>/brain/memory/`; rollups regenerate from still-visible facts, never persist sealed content) → memory import (paste ChatGPT/Claude export → extract → reconcile → synthetic `Memory Import` meeting anchor; minimal FE in brain-memory view). Gates: RED tests; **lock-security-reviewer** (new gated read + vault writes + import).

### Phase PR-5: L3 orchestration — size M (spec §L3; after PR-3 + PR-0)

`router.rs` (`route(RouterInput) -> RouteDecision`, additive over roles/postures) → escalation = ledgered `egress_log` row (`call_kind: "escalation"`) → grammar constraint for <512-byte schemas (`brain_heavy_grammar_enabled`, default false until real-Mac spike) → JIT ask retrieval behind `ask_jit_retrieval` (listing + **`get_meeting` tool gated by `meeting_is_unlocked` — lock-security REQUIRED**) → loop compaction (`compact_transcript` at 32k chars) + history char-budget (64k) → JSON one-retry + `supports_native_json()` capability flag → `prompts.rs` registry (re-export migration, zero behavior change). Gates: eval re-run for JIT answer-faithfulness; lock-security.

### Phase PR-6: L4 live incremental brain — size M (spec §L4 + WS-T T4; after T1, substrate per T3 verdict)

Novelty gatekeeper (`NoveltyState` pure fn on the tick thread — **composes with T1's VAD gate**: VAD gates the ASR decode, novelty gates the LLM bullets call; both read `ThermalGovernor`) → incremental bullets (`transcribe/bullets.rs::update_bullets`, prefix-prompted ≤3-new-bullets-or-NOTHING, `live_bullets` RAM + crash-table **blanked on seal**, reads gated) → boundary-timed card surfacing (mpsc queue, lull ≥8 s / sentence-final, 30 s force-emit, FE events unchanged) → bullets as Stage-1 note input (`SummarizeRequest.live_bullets`, rides `RedactingProvider`). **T4 decision:** if T3 adopted parakeet or T1 telemetry still shows redundant-decode pain, fold the LocalAgreement-2 commit-and-trim rewrite in here (it replaces `accumulate_live_caption`'s overlap-merge; `WakeDedup`/`step_manual_capture` assume overlapping tails — they get adapter shims). Gates: `cargo test --lib` + **real-Mac recording session** (bullets quality, boundary UX cannot be proven headless); lock-security (`live_bullets` seal semantics).

### Phase PR-7: L5 agents — size M–L (spec §L5; three mutually independent items, can split into 3 PRs)

Scheduled briefs (`brief_runner.rs`, structured schedule columns — no cron crate; propose-accept `brief_runs`; quiet-if-empty; FE `briefs/` feature) ⊥ MCP client (`connectors/mcp_client.rs` hand-rolled JSON-RPC over existing reqwest, HTTP+stdio, per-server consent rows, tools ONLY at Tier 3, descriptions sanitized + never interpolated into system prompts — **lock-security REQUIRED, new egress class**) ⊥ verify pass (deterministic `judge_with_detail` + idempotent `> [!verify]-` callout via the `enrich.rs` fence discipline, session `verify_cache` cleared on relock). Real-Mac: stdio MCP lifecycle + Obsidian callout rendering.

---

## Consolidated user-input checklist (the plan's external dependencies)

1. **PR-2 labeling** (~20 queries vs the dev vault) — the long pole for PR-3; ask for it right after PR-0 lands.
2. **sudo powermetrics runs** (T0 baseline, post-T1, T3 spike) — 3 sessions on the real Mac.
3. **T2 default flip** to `large-v3-turbo-q8_0` — explicit OK after seeing the A/B diff (+ ~900 MB download for existing users).
4. **Default decisions carried from the spec:** grammar flag stays false until a real-Mac spike; `ask_jit_retrieval` false until eval compares; `live_bullets_enabled` true-if-model-present; brief zero-egress flag deferred; MCP SSE deferred.
5. **parakeet adoption** (T3 verdict) + CC-BY-4.0 attribution line; FluidAudio sidecar only with new-dep approval.
6. **brain2 vs ClickUp Brain² naming** — unchanged hard blocker before any public copy.

## Release checkpoints (suggested)

- **0.8.1** after PR-0 + T1 (+T2 if approved): "cooler recording + safer brain" — user-feelable heat fix, worth shipping alone.
- **0.9.0** after PR-3/4/5: measured-retrieval brain.
- **0.10.0** after PR-6/7: live incremental brain + agents surface.

## Self-review notes

- Spec coverage: every spec item (P0.1–3, L1.1–L1.6, L2.1–3, L3 ×7, L4 ×4, L5 ×3) and every transcription item (T0–T4 + wake-listener pin) maps to a phase above; the spec's 11 open decisions appear in the user-input checklist or their owning phase.
- Interfaces consumed across phases are pinned: `GenOptions` presets + `user_turn_in_progress` (PR-0 → T1/PR-6), `ThermalGovernor` (T1 → PR-6), `RetrievalMode::Reranked` + artifact writer (PR-2 → PR-3/PR-5), `purge_chunks_tx` choke point (PR-3), `enrich.rs` fence discipline (PR-7).
- Deliberately NOT planned now (measurement-gated): exact fusion weights/k, reranker impl choice, JIT-vs-packed default, LocalAgreement adoption, parakeet adoption, CoreML/ANE escalations.
