<!-- Master plan — 2026-06-27. Authoritative roadmap for the brain2 RAG + local-reasoning + voice-trigger build. Supersedes ad-hoc planning; grounded in docs/research/2026-06-27-*.md. -->
# Master Plan — Murmur → brain2: intelligent RAG, local reasoning brain, voice-trigger

## North star (one paragraph)

Turn Murmur from "records a meeting → writes one isolated note" into a **local-first second brain**: a meeting flows through a **local reasoning brain** that figures out what context it needs, gathers it (semantic search over your own notes + external slices from Slack/Jira/Calendar), and hands a **complete context package** to Claude for a single, high-quality synthesis. The same brain + the same tool registry power three callers — the **app UI**, the **local MCP server** (Claude Desktop as a client of your encrypted memory), and **your voice mid-meeting** ("Claudku, zrób research o X" → it actually happens). Everything stays gated by the per-folder lock, on-device where it must be, redacted before any cloud egress.

## The unified architecture (what we're actually building)

```
                         ┌──────────────────────────────────────────────┐
   meeting / query  ───▶ │  LOCAL REASONING BRAIN (on-device LLM)        │
                         │   • entity/topic extraction (raw transcript)  │  zero egress
                         │   • plan: what context is missing?            │
                         │   • formulate fetches / parse voice → tool    │
                         └───────────────┬──────────────────────────────┘
                                         │  (calls the shared TOOL REGISTRY)
        ┌────────────────────────────────┼────────────────────────────────┐
        ▼                                ▼                                 ▼
  RETRIEVAL (local)              EXTERNAL FETCH (lazy)            ACTIONS
  FTS5 ∪ vector (RRF)            Calendar (EventKit, local)       create_task / reminder
  + entity-graph expansion       Slack / Jira (OAuth, on-demand)  research / recall_dossier
        └────────────────────────────────┼────────────────────────────────┘
                                         ▼
                         ┌──────────────────────────────────────────────┐
                         │  CLAUDE — single-shot hermetic SYNTHESIS      │  redacted + consented
                         │   (note / dossier / answer) over the package  │
                         └──────────────────────────────────────────────┘

  index is SOURCE-AGNOSTIC (`source_type`)  ·  every read gated by visibility_clause
  locked folders: embed-on-unlock (no plaintext vector at rest)  ·  one tool registry, three callers (UI / MCP / voice)
```

**Division of labor (decided):**
- **Local reasoning LLM** = the brain: pre-analysis, planning, voice-intent parsing, NER (which also closes the name-redaction hole). On-device, zero egress for decisions. Quality > speed (user accepts slower). Model = TBD by research (Phase-3 gate); leading candidate Qwen-32B-class / R1-distill; runtime MLX or llama.cpp; **bundled** (no Ollama runtime dependency).
- **Embedding model** = BGE-M3, **bundled** on-device, embeddings + lexical + ColBERT in one pass. Multilingual (Polish).
- **Claude (`claude_code`)** = final synthesis only, hermetic single-shot (it cannot embed — Anthropic has no embeddings API; and it stays sandboxed for note generation).

## Hard external unknowns (the real gates — resolved by research/spikes, not by `cargo test`)

1. **Best on-device reasoning LLM** for Apple Silicon + function-calling + Polish, + fine-tuning/LoRA recipe + runtime (MLX vs llama.cpp) + bundling cost. → **research (running now)**.
2. **sqlite-vec static-link under `bundled-sqlcipher-vendored-openssl` on macOS** — the real risk is the macOS `auto_extension` deprecation (#169), NOT SQLCipher (verifier: rusqlite 0.32 is on the good side, load_extension compiled in). → **~1-day build spike** before committing the vector layer.
3. **Polish recall** of BGE-M3 on real ASR transcripts → **bake-off on the live DB** (real Mac).
4. **Voice wake-word / intent detection** on the live stream — best on-device approach. → **research (running now)**.
5. **EventKit / Slack / Jira** — need a real Mac (TCC) + OAuth; integration-treadmill cost. Sequenced last, behind a proven retrieval layer.

## Phased roadmap (dependency-ordered; each phase is a verified, shippable increment)

### Phase 1 — Retrieval foundation  ⟶ BUILDING NOW
**Goal:** fix the proven-broken search and the confirmed correctness bug; make Ask cited. Zero new deps, fully headless-verifiable.
- Replace both `LIKE` paths with **FTS5/BM25** behind the existing `search()` / `search_visible()` signatures (`db.rs:406`, `db.rs:1521`), preserving the `visibility_clause` JOIN. External-content FTS5 table + sync triggers; sealed rows excluded.
- **Fix the unlock-set bug:** `ask_vault` (`commands.rs:1104`) and `pre_meeting_brief` (`commands.rs:1323`) → `build_vault_context_visible(..., live unlocked set)`. Today they silently omit session-unlocked folders.
- Raise the non-Ollama context budget (`vault_context.rs:25`) toward the model window; emit explicit `[[Title]]` citations.
- **Verify:** `cargo test --lib` green; RED-before-GREEN regressions (word-order symmetry; session-unlocked folder reappears in Ask); **lock-security-reviewer** (touches a visibility path).
- **Deliverable:** correct, complete, cited Ask across UI + MCP + Brief. ~80–90% of real-user retrieval value.

### Phase 2 — Vector layer + source-agnostic index
**Depends on:** Phase 1 + sqlite-vec build spike (#2) + Polish bake-off (#3).
- sqlite-vec build proof → bundled **BGE-M3** embeddings (fastembed/ort or llama.cpp) → **hybrid FTS5 ∪ vector, RRF-fused**.
- **Lock-safety (B1 embed-on-unlock):** never persist vectors for locked content; materialize in-memory on unlock, drop on relock. Gate KNN before search via visible-meeting-id set. (Verifier: invertible vectors = PII for sealed content.)
- **`source_type` column** (additive) + ingest seam → the index becomes source-agnostic groundwork.
- **GraphRAG-lite:** entity-anchored candidate expansion via `entity_mentions` — the differentiator no competitor (Granola/Otter/Meetily/Anarlog) ships.
- **Verify:** deterministic RRF/graph tests headless; recall/latency on real Mac; **lock-security-reviewer** (new index = new read surface).

### Phase 3 — Local reasoning brain
**Depends on:** model research (#1) + #4.
- Select model → integrate runtime (MLX or llama.cpp via Rust binding) → **bundled**, behind a new provider-like seam (separate from the cloud summarizer seam).
- **Early local NER/entity pass on the RAW transcript** (today entities are extracted post-summary, from the note, cloud-capable — `graph.rs:27-31`). This is genuinely new, zero-egress, and **also closes the name-redaction hole** (`redact.rs`).
- **Constrained/structured decoding** for reliable tool-call JSON (no fine-tune needed for parsing). Fine-tune (LoRA) later from collected usage = flywheel.
- **Verify:** intent-parse + plan unit tests; honest "needs real Mac for latency".

### Phase 4 — Pre-analysis pipeline (the "smart assembly")
**Depends on:** Phase 2 + Phase 3.
- New front of the note pipeline: local pre-analysis (entities/topics) → retrieve related local context → assemble package → claude single-shot synthesis. Grounds each new note in related prior notes/decisions ("last time you decided X").
- **Verify:** prompt-builder unit tests like `digest.rs`; the package assembly is deterministic + gated.

### Phase 5 — Tool registry + MCP expansion (the "three callers" seam)
**Depends on:** Phase 2 (+3 for natural-language dispatch).
- One **tool registry**: `semantic_search`, `recall_dossier`, `get_open_commitments`, `research`, `create_task/reminder`, `fetch_external`. Each a thin, visibility-gated reader/action.
- Expose via the **local MCP server** (`mcp.rs`) — the privacy-correct inverse of Granola/Otter cloud MCP. Adds tools #4–#n.
- **Verify:** MCP tests; every tool routes through the gate by construction.

### Phase 6 — External sources (multi-source enrichment)
**Depends on:** Phase 2 (`source_type`) + Phase 5 (tool registry) + real Mac/OAuth.
- **Calendar (EventKit) first** — zero OAuth, zero egress, attendee emails = stable entity IDs. Swift sidecar mirroring `sysaudio.swift`.
- **Slack / Jira** later, behind `source_type` + the tool registry, **lazy-fetch** (don't mirror everything). Honest: each = OAuth + perpetual connector maintenance; the Mem/Rewind landfill risk → only after retrieval is proven.
- **Verify:** real Mac; egress is redacted+consented; lock-security on any aggregated egress.

### Phase 7 — Voice-trigger "Hej Claude" 🎙️
**Depends on:** Phase 3 (brain) + Phase 5 (tool registry) + #4.
- Local **wake-phrase + intent detector** on the live transcript stream (reuse `transcribe/live.rs`, `onLiveCaption`, VAD `transcribe/vad`, existing voice-trigger infra).
- Capture utterance (VAD pause) → local brain parses → dispatch tool registry **async** (recording never pauses) → result lands as a side card (with undo) **and** woven into the note's "Assistant actions" section.
- Soft confirmation ("⚡ Heard: research X — running, cancel ↩"), high-precision wake phrase. Detection/parse = local/zero-egress; execution may egress (deliberate user command).
- **Verify:** wake-detection precision/recall on recordings; honest "real-mic + signed build" for the full loop.

## Cross-cutting (every phase)
- **Lock model is load-bearing:** every new read/index/export gated by `meeting_is_unlocked`/`visibility_clause`; every new seal verify-before-destroy; **lock-security-reviewer is the required gate** on anything touching content/index/egress.
- **Redaction:** name-redaction (Phase 3 NER) becomes load-bearing once we feed Claude richer cross-source, name-dense context.
- **No commit/PR without explicit user OK.** Author = QueaT; merge via PR to `murmur`; `com.meetnotes.app` immutable.
- **Build in verified increments** — adversarial-verifier owns PASS/FAIL; implementer never self-certifies.

## What's running right now
- **Phase 1 build** (FTS5 + unlock-fix + cited Ask) → rust-tauri-dev build → adversarial-verify → lock-security-review.
- **Research** (background): best on-device reasoning LLM (Apple Silicon + function-calling + Polish) + fine-tuning/LoRA + voice wake-word/intent → pins Phases 3 & 7.
