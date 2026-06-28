<!-- Design blueprint (architect + adversarial critique, 2026-06-28). The unified local-brain-as-first-layer. Pairs with docs/research/2026-06-28-local-brain-delivery.md (how to ship the model) + docs/PLAN-brain2-rag-voice.md. -->
# Design: the local brain as the "first layer" — orchestration over the existing seams

## Verdict (architect + adversarial critique)
The **shape is right**: the brain is an *orchestration layer*, not a new component — it slots between the transcript and the seams we already merged, and with `StubReasoner` every flow degrades to **exactly today's behavior**, so it's headless-testable before any ML model exists. The real model is a one-line `Arc::new` swap.

But the critique found three things that change the *plan* (not the shape):
- 🔴 **CRITICAL lock-safety FAIL in the flywheel** — the naive `correction_log` capture + export is an **ungated, un-sealable plaintext shadow copy of sealed content** (bug class #2). Must be fixed before ANY capture is wired.
- 🟡 **The headline "brain decides what to fetch / whether Claude is needed" is unwired AND unprovable headless** — constrained decoding fixes *format*, not *decision quality*. Replacing the working, gated, tested deterministic retrieval with a non-deterministic brain plan is the **agentic-RAG-overkill trap**. Prove value on a real model before swapping.
- 🟡 **Fine-tuning is premature + wrong-signal + dangerous** (the substrate IS the leak). Defer.

→ **The plan: ship the safe, model-free, egress-free plumbing NOW (it hardens existing code + makes the brain a clean swap); fix the flywheel before capture; prove the brain's decision-value on a real model before it replaces the deterministic path.**

## The architecture (the shape — confirmed sound)

The brain is an orchestration layer with **3 flows over the existing seams**, plus 3 new Rust files (`tools.rs`, `orchestrate.rs`, `voice_action.rs`); `reason.rs`, `audio/wake.rs`, the gated readers, and the `correction_log` machinery are *wired*, not rewritten.

- **Flow A — meeting pre-analysis** (replaces the *deciding* step inside `pipeline.rs::build_grounding_context`): transcript excerpt → `reasoner.structured(PRE_ANALYSIS_SCHEMA)` → retrieval plan → `tools::execute_tool` (gated) → assemble corpus → delegate synthesis to Claude. **Stub-shim:** when the reasoner is the stub, fall through to today's `salient_query + build_related_context` — byte-identical.
- **Flow B — voice action** (upgrades `parse_voice_intent` + wires `detect_wake` into `transcribe/live.rs`): live tail → `detect_wake` (pure/fast gate) → on a hit, off-thread `reasoner` plan → `voice_action::handle` → `tools::execute_tool` → result woven into the note. Recording never pauses.
- **Flow C — structured extraction** (replaces brittle `rfind('}')` + a cloud round-trip in `graph.rs`/`timeline.rs`): `reasoner.structured()` + `reason::parse_first_json` (already robust + unit-tested).

**The unified tool registry** — `tools::execute_tool(call, db, unlocked)`: one function the brain, the MCP server, and commands all call. The `unlocked: &HashSet<String>` parameter is **non-optional** → the lock gate is enforced by the function signature. The 6 MCP arms become thin delegators (pure refactor, zero behavior change).

**Why it's safe by construction (Flow A egress):** the assembled corpus egresses through the **same** `make_provider` → `RedactingProvider` + fail-closed consent gate that today's grounding already uses (`summarize/mod.rs:63-69`) — no new egress class. The brain's *decisions* run on-device and never egress.

## 🔴 The flywheel lock-safety fix (REQUIRED before any capture)

As designed, `correction_log` (`storage/db.rs:259`) has `kind, input, model_output, final_output, accepted, owner_id, created_at` — **no `meeting_id`/`folder_id`**. So:
- It can't be filtered by `visibility_clause` or scoped by folder.
- The seal/blank path (`blank_sealed_notes_in_folders`) never touches it → entity names + transcript excerpts captured at generation time stay in **plaintext forever**, readable with the DB open (defeating the per-folder seal's core property), and the proposed `export_correction_log` routes them to a JSONL/LoRA script (a **new exfil surface**).

**Fix (additive, before any capture is wired):**
1. Add `meeting_id TEXT` (+ derivable `folder_id`) to `correction_log` via `add_column_if_missing` (`db.rs:212`) — additive, guarded.
2. Add `correction_log` to **every seal/relock/reblank/delete purge path** (blank or delete its rows in the same transaction as the plaintext blanking — like `vec_chunks`/`note_chunks` today), OR store only **non-content signal** (the decision/plan structure, never raw excerpts/names).
3. **Gate `list_corrections`/any export** with the `unlocked` set + route through the redaction firewall + consent gate — it is a content export path and the lock-model hard rule applies.
4. Until 1–3 land, **do not wire any capture**. (Today `log_correction`/`list_corrections` are test-only — no prod caller — so there is nothing to leak yet. Keep it that way until gated.)

This goes through `lock-security-reviewer` as a required gate.

## The plan (sequenced by the critique)

### 3b.0 — Safe plumbing, model-free, headless-verifiable, egress-free (SHIP NOW)
The "smallest version that delivers value" — needs no model, touches no egress, hardens existing code, and makes the eventual brain a clean swap:
1. **Unified tool registry** — extract `mcp.rs::dispatch_tool` → `tools::execute_tool(call, db, unlocked)`; the 6 MCP tools become thin delegators (all 6 MCP tests stay green unchanged). The non-optional `unlocked` param is the structural gate.
2. **Parse-robustification** — swap the brittle `rfind('}')` slices in `graph.rs`/`timeline.rs` for `reason::parse_first_json` (already robust + unit-tested). Fixes a real latent bug (stray braces / two objects) independent of any model.
3. **Wire `detect_wake` into `transcribe/live.rs`** — pure/fast wake gate on the live tail (the brain only spawns off-thread on a hit). Headless-testable matcher (already merged); real-mic precision is the Mac step.

All three are deterministic, gated, and `cargo test --lib`-verifiable. Each is a small PR through the normal build→adversarial(→lock-review for #1) gate.

### 3b.1 — The real brain (model-gated)
1. **Build-proof spike first** (see `docs/research/2026-06-28-local-brain-delivery.md`): `mistral.rs` (candle, NOT onnxruntime → no sherpa ORT collision) links + runs in-process alongside whisper+sherpa on macOS, with a pinned-arch GGUF (avoid the #2125 unknown-arch `unwrap()`).
2. `MistralReasoner: LocalReasoner` behind the merged seam; `AppState.reasoner` swap; `spawn_blocking` for the sync inference (like Whisper at `pipeline.rs:346`).
3. Wire `orchestrate.rs` Flow A/C with the stub-shim retained as the fallback.
4. **Prove decision-value on real data** before letting the brain *replace* the deterministic retrieval — keep `build_related_context` as the floor; the brain has to beat it.

### 3b.2 — Flywheel + fine-tune (only after the lock-fix + evidence)
- Land the flywheel lock-fix (above), THEN wire capture (with `accepted=false`-on-user-edit as the real signal, not `accepted=true`-on-generation).
- LoRA only once there are hundreds–thousands of clean correction pairs and prompt-engineering is shown to be the bottleneck. Hyprnote's Qwen3-1.7B fine-tune is the proof the path works (`delivery` doc), but it's a later investment.

### Deferred (declared-but-don't-build-yet)
- **"Brain decides Claude isn't needed"** (`answer_locally`) — unwired, risks degrading note quality, prove-or-drop on a real model.
- **Voice external actions** (Slack dispatch) — the moment external dispatch lands it's a NEW egress class that MUST route through the firewall + a consent gate; flagged now.

## Honest bar
- **Headless-verifiable now:** 3b.0 entirely (registry refactor, parse-robustification, wake-wiring logic), the flywheel lock-fix + its gate tests, all stub-degrades-to-today paths.
- **Real-Mac-gated:** Metal inference in `MistralReasoner`, real-mic wake precision, the brain's actual decision quality, Touch ID with the reasoner in the loop, model-download UX.
- `com.meetnotes.app` immutable; no Ollama import; additive migrations only.

## Sources
Design + adversarial critique (workflow `wd6ijn1w6`, 2026-06-28). Code: `reason.rs:21/44/82`, `pipeline.rs:717/346`, `mcp.rs:300-429`, `graph.rs:36`, `timeline.rs:54`, `storage/db.rs:259/492/517/1736`, `summarize/mod.rs:63-69`, `summarize/redact.rs:62`, `audio/wake.rs:78`, `state.rs:40`. Delivery decision: `docs/research/2026-06-28-local-brain-delivery.md`.
