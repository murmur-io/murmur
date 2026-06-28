<!-- The master execution plan to take the brain2 work from v0.5.0 (seams + stubs) to a FINISHED product — every stub replaced by a real component. Decisions locked 2026-06-28. Pairs with DESIGN-local-brain-orchestration.md + research/2026-06-28-local-brain-delivery.md. -->
# Master plan — from v0.5.0 seams to a finished product (no stubs, no mocks)

The brain2 work shipped in v0.5.0 as **seams + stubs**. This plan replaces **every stub with a real component** and wires the full end-to-end flow. Target: **v0.6.0**.

## Locked decisions (2026-06-28)
1. **Brain model:** off-the-shelf **11–14B** (Bielik-11B-v3.0-Instruct, Polish-native, Llama-arch, pinned to avoid mistral.rs #2125; Qwen3-14B alt). Fine-tune later, evidence-gated.
2. **NER (name-redaction):** **dedicated GLiNER** (`gline-rs`/`ort`). Accept the 2×-ONNX-runtime risk → **isolated build-proof FIRST** (does gline-rs's ORT link alongside sherpa's static ORT on macOS?). If it won't link, fall back to brain-does-NER (constrained mistral.rs).
3. **External sources:** **Calendar first** (local EventKit, zero-OAuth); Slack/Jira only after retrieval value is proven.
4. **Cadence:** build everything **headless + autonomously**; **@-flag the user only when a step needs his Mac** (model download + inference eval, real-mic precision, Touch ID, signed build).

## Every stub → real (the definition of "done")
| Stub today | Becomes | Phase |
|---|---|---|
| `StubReasoner` | `MistralReasoner` (mistral.rs + Bielik-11B) | B |
| `StubEmbedder` | real on-device embedder (BGE-M3/fastembed) | C |
| `NoopNameRedactor` | real **GLiNER** redactor | D |
| deterministic salient-query | brain-decided context (with deterministic floor) | B |
| semantic flag OFF | ON, after embedder + bake-off | C |
| wake matcher (unwired) | wired into `transcribe/live.rs` + Whisper bias | E |
| no voice actions | unified `tools.rs` registry dispatch | A+E |
| `correction_log` (test-only, lock-unsafe) | lock-safe + capture + LoRA | F |
| `source_type` seam only | Calendar → Slack/Jira | G |

## Phases (each = build → adversarial → lock-review where relevant → PR-merge)

### Phase A — Safe plumbing (headless, no model, egress-free) — BUILD NOW
1. **Unified tool registry** — extract `mcp.rs::dispatch_tool` → `tools::execute_tool(call, db, unlocked)` (non-optional `unlocked` = structural gate); 6 MCP tools become thin delegators (all 6 tests green unchanged). + lock-review.
2. **Parse-robustification** — `graph.rs`/`timeline.rs` brittle `rfind('}')` → `reason::parse_first_json`. Fixes a latent bug, no model.
3. **Wire `detect_wake` into `transcribe/live.rs`** — pure/fast wake gate; brain spawns off-thread only on a hit. (Real-mic precision = Mac.)

### Phase F0 — Flywheel lock-safety fix (headless) — DO EARLY (it's a lock fix + unblocks capture)
- Add `meeting_id`/`folder_id` to `correction_log` (additive `add_column_if_missing`); add it to **every** seal/relock/reblank/delete purge path (same tx as plaintext blanking); gate `list_corrections`/export through the unlock set + firewall. **No capture wired until this lands + passes lock-review.**

### Phase B — The brain runtime (build-proof headless → impl headless → eval = @Mac)
1. **Build-proof spike** (isolated worktree): `mistralrs` links + the `gguf_locally` example compiles in-process alongside sherpa+whisper on macOS; bundle/compile cost acceptable. ← may need **@Mac** to run the example.
2. `MistralReasoner: LocalReasoner` (compiles headless; download-on-first-run like `transcribe/model.rs`; `spawn_blocking` for sync inference; pinned arch).
3. Wire `orchestrate.rs` Flow A/C with the deterministic stub-shim as fallback.
4. **Eval decision-value on real data (@Mac)** — the brain must beat the deterministic floor before it replaces it.

### Phase C — Real embedder + semantic ON (build-proof headless → impl → bake-off = @Mac)
1. Build-proof the embedder runtime (fastembed/candle BGE-M3) — link check headless.
2. Real `Embedder` impl; **embed-on-unlock** (close the FTS/vector recovery asymmetry); `EMBED_DIM` migration if width changes.
3. **Bake-off (@Mac)** FTS-only vs hybrid on the real vault (`docs/RAG-BAKEOFF.md`); flip `semantic_search_enabled` default on only if it wins.

### Phase D — GLiNER name-redaction (build-proof headless → impl → eval = @Mac)
1. **Isolated build-proof**: `gline-rs`/`ort` links alongside sherpa's static ORT (the real risk). If conflict → fall back to brain-NER.
2. Real `NameRedactor` (GLiNER) behind the seam; close the cloud name-leak (the one documented privacy gap).
3. **Polish-NER quality eval (@Mac)**.

### Phase E — Voice end-to-end (wiring headless → real-mic = @Mac)
1. Whisper `set_initial_prompt` wake-bias; intent → action via `tools.rs`/`voice_action.rs`; result woven into the note's "assistant actions".
2. **Real-mic precision + the live loop (@Mac, signed build for Touch-ID-adjacent paths).**

### Phase G — Calendar source (sidecar headless build → TCC = @Mac)
- EventKit Swift sidecar (like the audio helpers) → meeting context (attendees/title/agenda) into the pipeline via `source_type`. **TCC permission test = @Mac.**

### Phase H — UX
- RAM-gated model-download picker (GPT4All/Jan-style); the "assistant actions" surface in the note; consent flows for any new egress.

### Phase I — Release v0.6.0
- Bump → universal build → **sign → notarize → publish (@Mac, the user's interactive auth)**.

## Honesty bar
Headless I verify: logic, lock-safety, no-regression, compile+link of the real impls. **@Mac-only:** model inference quality/latency, Polish NER/embedding recall, real-mic wake precision, Touch ID, the signed release. Each flagged at its step. `com.meetnotes.app` immutable; additive migrations only; no Ollama hard-dep.
