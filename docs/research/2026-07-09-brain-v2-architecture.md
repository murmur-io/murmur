<!-- Generated 2026-07-09 via /research (2 code-explorer traces + 2 murmur-researcher angles: ClickUp Brain² competitive + academic/industry best practices). Pricing/versions = point-in-time. -->
# Research: Brain v2 — architecture analysis, problems, and the proposal

**Scope:** deep analysis of the brain as shipped in 0.8.0 (local model flow, live reactions, live questions, cloud flow, context management), measured against Anthropic's published engineering guidance + the academic memory/retrieval/streaming literature + ClickUp Brain² (GA 2026-06), ending in a Brain v2 architecture proposal.

---

## TL;DR / Verdict

**Murmur already owns every primitive the literature says matters** — lexical index (FTS5), dense index (e5 + sqlite-vec), entity graph, bitemporal facts, user memory, an agentic tool loop with structural tier gating, small local models, a cloud seam with redaction + consent + egress ledger. **Brain v2 is an orchestration, consolidation, and quality-measurement problem, not a missing-component problem.** The three structural deltas vs state of the art:

1. **Retrieval quality is unmeasured and un-reranked.** Hybrid RRF exists (A-grade design) but: no reranker (audit grade F, still true), char-based whole-meeting chunks with zero contextual augmentation, rank-fusion instead of score-fusion, k=60 uncalibrated, and the eval harness has never produced a number. Anthropic's own numbers say contextual augmentation + reranking cuts retrieval failure 67%; LongMemEval's three fixes (session decomposition, fact-augmented keys, time-aware expansion) map 1:1 onto tables we already have.
2. **Context management is stuff-and-pray, not engineered.** The Ask corpus packs up to 200k chars into the system prompt (vs Anthropic's just-in-time retrieval + compaction guidance); the agentic loop transcript grows monotonically with no compaction; the live buffer is a naive word-overlap merge with no rolling summary state; the memory brief is always-injected regardless of relevance; @brain history has a turn cap but no token budget. Small local models get the worst of it: NoLiMa shows semantic-only retrieval into long small-model context is the single worst combination (effective context for 1.7B/4B ≈ 8–16k tokens).
3. **The live brain is a fixed-cadence scanner, not a gated incremental summarizer.** Reactions fire every ~21s on a 600-char tail through an unconstrained-JSON 1.7B call; there is no novelty gatekeeper, no boundary-aware timing, no incremental running state, no priority of user questions over background scans, no token cap or wall-clock timeout on user answers. The published production pattern (incremental bullets + cheap classifier gatekeeper + empty-response-if-nothing-new) is directly applicable and *reduces* RAM/latency.

**Vs ClickUp Brain²:** architecture parity is closer than the marketing suggests — their Context Engine ≈ our hybrid retrieval + graph + facts; their memory ≈ our `user_facts` (we're even ahead on auditability and bitemporality). The real deltas are **memory import** (they import ChatGPT/Claude memories), **schedule-as-a-first-class-agent-trigger**, and **MCP-as-client** connector fabric. Their complaint profile ("confident wrong answers on messy data") confirms the verify-pass remains our headline differentiator. **Nobody occupies proactive-live-on-device** — Fireflies is the only proactive live competitor and it's bot-dependent + cloud; Limitless was acquired by Meta and Rewind is dead, so the local-first memory niche just vacated.

**The verdict in one line:** keep the substrate (storage, lock model, provider seam, tiering — all A-grade), rebuild the *middle* — indexing/consolidation as measured workflows, context assembly as engineered budgets, the live loop as a gated incremental summarizer — and add the three cheap ClickUp-parity moves (memory import, scheduled briefs, MCP client) plus the verify pass.

---

## Part 1 — The brain as it exists (code-grounded)

### 1.1 Layered map (as-is)

```
INGEST    audio (cpal + SCK sidecar) → dual-stream merge → whisper.cpp (VAD, 120s windows)
          → TranscriptFeed{retrieval_text, summary_text, labeled}         pipeline.rs
INDEX     e5-small 384-dim (candle) → note chunks (800 chars) + transcript turns
          (1000 chars, 150 overlap) → sqlite-vec KNN; FTS5 (BM25, diacritics-folded);
          deterministic NER → entities/entity_mentions; facts + user_facts (bitemporal,
          reconcile pipeline)                                              embed.rs, db.rs, facts.rs
RETRIEVE  search_visible (FTS) / search_hybrid_visible (RRF k=60: FTS + KNN + entity leg)
          — every branch visibility_clause-gated, note bodies double-gated
                                                                           tools.rs, db.rs, vault_context.rs
REASON    LocalReasoner trait: MistralReasoner (GGUF, MODEL_CACHE cap 2, refuse-not-evict,
          RAM guard ×1.5) | CloudReasoner (→ make_provider → consent → RedactingProvider)
          | StubReasoner | AfmReasoner                                     reason.rs, reason/mistral.rs
ACT       two-stage notes (Stage-1 isolated gen → Stage-2 additive enrich lanes A/B);
          agentic Ask (run_agentic_loop + GatedToolExecutor); MCP server; obsidian export
                                                                           pipeline.rs, enrich.rs, agent.rs, mcp.rs
```

### 1.2 Live reactions (100% local, never cloud)

- Live caption thread ticks every **3s**, snapshots a **14s** overlapping audio tail, transcribes with the Fast/greedy profile, merges into `state.live_transcript` (`Mutex<String>`, **16k char cap**, mic-only — far side is batch-only after Stop) via word-level suffix-prefix overlap removal (`live.rs:merge_live_caption`).
- Every **7th tick (~21s)**, if a `reactions_busy` AtomicBool is clear, a worker thread runs `brain_reactions.rs:reactions_scan`: takes the last **600 chars** of the buffer, matches visible entity names, calls the **light reasoner only** (`reasoner.light()` = qwen3-1.7B or stub — structurally never cloud) with `GenOptions::light_extraction()` (128 tokens, temp 0.2, thinking off), recovers triples via `parse_first_json` (schema-in-prompt, **not grammar-constrained**), then a **pure deterministic** `reconcile_facts` produces contradiction WhisperCards. Session-dedup by `entity|predicate|old_quote` HashSet.
- `proactive.rs` is fully deterministic (zero LLM): delta-tracked scan every ~30s, cooldown ≥120s, recency-decay scoring over commitments/facts/FTS terms.

### 1.3 Live questions (the current-first cascade)

Three entries (wake word, button+voice capture, typed @brain) converge on `live.rs:run_assistant_query`:

- Scope = `fe_meeting_id → focus_meeting → current_meeting` (the wrong-meeting bug is mitigated but the durable per-thread binding is still FE-only).
- Reasoner = `Role::Live` resolution (roles/postures). **If the target is local-GGUF-only, the agentic cascade is skipped entirely** and the deterministic floor (`run_informational` → keyword check → `handle_voice_action` fan-out) runs instead — the agentic loop only ever executes on `CloudReasoner` (incl. loopback Ollama).
- Cloud cascade: shared system prompt = persona + **≤6k-char live-buffer tail** + **≤6k-char typed notes** + **≤2k-char always-injected memory brief**; then Tier 1 (no tools, 2 steps) → `__ESCALATE__` → Tier 2 (vault search tools, 4 steps) → Tier 3 (connectors/web, 3 steps). Tier gating is **structural** (`GatedToolExecutor::specs()` + re-checked allowlist per call) — this part matches best practice. Cloud egress passes consent → NER+regex redaction → egress ledger.
- Loop mechanics: transcript string grows monotonically ("User request: …" + `[tool result]` blocks truncated at **4k chars each**); no compaction, no total budget, **no token cap on the answer** (`GenOptions::default()`), **no wall-clock timeout**, no in-flight dedup of concurrent turns, no app-level priority between a background reactions scan and a user question sharing the same runtime.

### 1.4 Ask-my-vault & notes (non-live)

- `ask_vault`: hybrid retrieval → `pack_meetings` corpus (**4k chars for Ollama, 200k chars for cloud**) pre-stuffed into the prompt + agentic loop (6 steps, `AssistantScope::Full`) with deterministic corpus-pack floor fallback.
- Notes (0.8.0 two-stage): Stage-1 generates from **this meeting only** (`related_context = None` — the cross-meeting-bleed fix, structurally sound); Stage-2 additive lanes: A = zero-egress `[[links]]` via task-free `reference_gist`, B = connector context callout; both idempotent, byte-exact-undo (`enrich.rs`). Weak providers (local/ollama) get a 3k-char link-only grounding budget — the decimation the small-model literature retroactively validates.
- Conversation layer: backend stateless per turn; FE resends 12-turn history; threads persist in `assistant_interactions` for rehydration only.

### 1.5 Provider capability matrix

| Provider | Streaming | Native tools | Native JSON | Redaction | Ledger |
|---|---|---|---|---|---|
| claude_code (CLI) | no | no | no (schema-in-prompt) | yes | yes |
| anthropic (API) | no | no | no | yes | yes |
| gateway (OpenAI-compat) | no | no | **yes** (json_schema) | yes | yes |
| ollama loopback | no | no | no | no (on-device) | no |
| local GGUF (mistralrs) | no | no | no (JsonSchema constraint abandoned — context overflow on Bielik-11B) | n/a | n/a |

No backend streams; only gateway has constrained decoding; the agentic loop is JSON-in-prompt everywhere, with no retry on malformed JSON (a malformed step breaks the loop).

---

## Part 2 — What best practice says (research synthesis)

### 2.1 Anthropic's guidance (all fetched)

- **Workflows vs agents**: predictable-step tasks (note gen, digest, extraction, consolidation) should be *workflows*; reserve the agent loop for genuinely open-ended queries. "Add multi-step agentic systems only when simpler solutions fall short."
- **Context engineering**: context = a depleting attention budget ("context rot"). Named techniques: compaction, structured note-taking outside context, **just-in-time retrieval over lightweight identifiers** (vs pre-stuffing 200k-char corpora), sub-agent context isolation.
- **Verification hierarchy**: rules-based feedback > visual > LLM-as-judge ("generally not very robust") — a direct endorsement of Murmur's deterministic verify approach.
- **Tool design**: fewer, higher-level tools; token-efficient returns; meaningful text over UUIDs; response-format enums.
- **Contextual retrieval numbers**: contextual embeddings −35% retrieval failures; + contextual BM25 −49%; **+ reranking −67%**. Under ~200k tokens, skip RAG and stuff — i.e., *within one meeting stuff the transcript; retrieval is a cross-meeting problem*.

### 2.2 Memory science

- The systems that win on LoCoMo/LongMemEval are **extraction-and-consolidation pipelines over structured stores** (Mem0: +26% vs OpenAI memory, >90% token savings; graph variant adds only ~2%), not raw-transcript vector dumps and not agent-self-managed memory. Murmur's facts/entities/FTS is already this shape.
- **LongMemEval's three validated fixes** (all cheap for us): session decomposition (index topic-segments, not whole meetings), fact-augmented index keys (prepend extracted facts/entities to chunks), time-aware query expansion (meetings are timestamped).
- **Generative-agents consolidation recipe**: score = recency (0.995/hr decay) + LLM importance (1–10 at write) + relevance; periodic *reflection* synthesizes higher-level memories. Cheap to implement as a SQLite view + periodic job.
- Effective small-model context: RULER — only half of models hold up at their claimed 32k; NoLiMa — 11/13 models drop below 50% of baseline at 32k without lexical overlap; plan for **~8–16k effective** on Qwen3-1.7B/4B and keep lexical/entity anchors in retrieval (FTS-first is scientifically correct here).

### 2.3 Real-time / streaming patterns

- The deployed-production template (arXiv 2510.06677): **prefix prompting** — send previous bullets + recent turns, model emits *only new bullets or empty*; a ~100M classifier (F1 0.895, p50 20ms) gates trivial output. p50 600ms.
- Proactive timing: lightweight signal detectors match LLM wake-deciders (arXiv 2605.30152); **boundary-timed interventions are accepted, mid-task ones dismissed** (arXiv 2601.10253); the "Goldilocks window" is a tunable policy, not a fixed cadence.
- Cascades: FrugalGPT (up to 98% cost cut at held quality), RouteLLM (routers transfer across model pairs). For us "cost" = RAM + latency + privacy egress; the architecture is identical: 1.7B classify/filter → 4B extract/summarize → cloud (redacted, visible) on escalation.
- Small-model tool use: **Qwen3-1.7B scores 7.8% on BFCL** (function calling), 4B ~40% even with specialized training — the current code's refusal to run the agentic loop on local GGUF is *scientifically correct*; the fix is honest marketing + a constrained few-tool grammar path for the 4B, not "turn the loop on".

### 2.4 ClickUp Brain² (GA 2026-06) & the class

- Brain²: event-sourced Context Engine, hybrid vector+graph retrieval, permission-aware; **editable + importable memory** (import from ChatGPT/Claude — their cold-start killer); multi-model routing (invisible plumbing); **MCP as connector fabric** (1,000+ tools); agents with **schedule as a first-class trigger** (Autopilot/Super Agents, Ambient Answers); "verified answers" via sandboxed compute; $9–28/user/mo (monthly $18/$68), every-seat billing. Complaints unchanged: confident wrong answers on messy data, agents need tight scoping.
- Live in-meeting AI is **commodity and pull-based** everywhere (Zoom free tier, Teams, Granola "Ask", Otter's voice agent). The only proactive-push competitor is Fireflies Live Assist (dynamic suggestion cards) — bot-required, cloud-processed. **Proactive + on-device + zero-egress is an empty position we already hold.**
- Meta acquired Limitless (Dec 2025), Rewind shut down — the local-first personal-memory niche is vacated. Windows Recall's failure shows "local + encrypted" doesn't sell alone; a killer use-case + visible trust story does.
- Legal: Otter (4 consolidated federal suits) and Fireflies (2 BIPA suits) over bot recording — our bot-free on-device capture is a defensive asset.

---

## Part 3 — Problems (prioritized)

**P0 — correctness/privacy bugs (fix regardless of v2):**

1. **`vault_titles` egress leak** — every `.md` stem (incl. auto-created `[[Person Name]].md`) reaches cloud providers past the NER layer on every cloud summarize (`pipeline.rs` → `redact.rs` filters only regex-altered titles). Confirmed in the 07-04 truth audit, still unpatched.
2. **StubReasoner echo surfaces as a real @brain answer** (`run_informational` lacks the `id()=="stub"` guard that `orchestrate.rs` has) — fresh installs get `[stub-reason] system=N chars…` as an "answer".
3. **No token cap / no wall-clock timeout on live user answers** (`GenOptions::default()` in the question path; loop bounded by steps only) — a chatty local generation occupies Metal indefinitely; contributes to the OOM/latency incident class.
4. **No priority or cancellation between background reactions and user questions** sharing `brain_rt`; no in-flight dedup of concurrent assistant turns.

**P1 — context-management debt (the "not Anthropic-level" core):**

5. Corpus **pre-stuffing** (200k chars) instead of just-in-time retrieval; agentic-loop transcript grows with no compaction or total budget; @brain history capped by turns, not tokens.
6. **Memory brief always injected**, never relevance-filtered; no consolidation/reflection pass, no importance/recency scoring — facts accumulate but never synthesize.
7. Live buffer = naive word-overlap merge, no rolling incremental state; the 600-char reaction window and 6k inject caps are unprincipled constants; live is mic-only (far side invisible to the live brain).
8. Prompts are string literals scattered in `live.rs`/`agent.rs` (EN+PL mixed), no registry, no versioning, duplicated wake-phrases across files.

**P2 — retrieval quality debt:**

9. **No reranker** (audit F, still true); RRF k=60 (TREC-scale) uncalibrated; rank-fusion where score-fusion measures ~6% better.
10. Char-based chunking, whole-meeting index granularity, **zero contextual augmentation** (no title/date/attendees/facts on chunks), no time-aware query expansion.
11. **Evals exist but never run** — bakeoff/diarization harnesses have zero committed result artifacts; retrieval and answer faithfulness are unmeasured on a real vault; citations never checked against cited content (C−).

**P3 — reliability/robustness debt:**

12. JSON-in-prompt everywhere with no retry (a malformed step kills the loop); only gateway has constrained decoding; local grammar constraint abandoned instead of scoped.
13. Agentic loop silently unavailable on local GGUF (correct per BFCL science, but undocumented and marketed otherwise).
14. `commands.rs` god-file (>8k lines) as the single coupling point.

**P4 — capability gaps vs ClickUp Brain²:**

15. No memory import (their cold-start killer is a paste-and-extract away for us, fully local).
16. No user-facing scheduled agents (we have digest/proactive machinery, no generality).
17. MCP server only — no MCP *client* (their 1,000-tool fabric pattern).
18. Verify-pass (✓ confirmed in PROJ-123) still unshipped — the headline trust differentiator their complaint profile begs for.

---

## Part 4 — Brain v2 proposal

**Design stance:** *workflow-first, agent-when-needed; measured, budgeted, gated.* Keep the substrate untouched (SQLite canonical, lock gating, provider seam + redaction + ledger, structural tiering, two-stage notes). Rebuild the middle in five layers:

### L1 — Indexing & retrieval ("the consolidation brain") — effort M, highest evidence-per-effort

- **Topic-segment indexing**: segment transcripts at topic boundaries (deterministic: speaker-run + lull + lexical-shift heuristic) and index segments, not whole meetings (LongMemEval session decomposition).
- **Deterministic contextual augmentation**: prepend `title | date | attendees | active facts` to every chunk for BOTH FTS and embedding (Anthropic contextual retrieval mechanism at zero LLM cost — our facts/entities make the "situating context" templatable).
- **Time-aware query expansion** (parse "last week/w zeszłym tygodniu" → date constraint on `search_hybrid_visible`).
- **Score-fusion instead of rank-fusion**; calibrate k on the eval set.
- **Rerank stage behind a trait** — bake-off prompted Qwen3-1.7B pointwise (already resident) vs bge-reranker-v2-m3 on M-series; ship whichever wins latency×recall; Ask-only (too slow for live).
- **Gate: the eval harness runs.** Fixed ~20-query set over a real dev vault; recall@5 per config + RAGAS-style judged faithfulness on Ask answers; a committed results artifact per architecture change. No retrieval change merges without a number.

### L2 — Memory — effort S–M

- **Consolidation/reflection job** (generative-agents recipe): score facts/interactions by recency-decay + importance + relevance; periodic local-model reflection synthesizes per-entity and weekly rollup summaries as *retrievable objects* (GraphRAG's one killer idea; digest/entity pages are 70% there). Output = plain `.md` + wikilinks (Obsidian-native, A-MEM-validated).
- **Relevance-filtered memory brief**: retrieve top-k user facts against the query instead of always injecting the full 2k brief.
- **Memory import** (ClickUp parity, S): paste ChatGPT/Claude exported memories → local extraction into `user_facts` → auditable in the existing brain-memory view, lock-gated, zero egress.

### L3 — Reasoning & orchestration — effort M

- **Formalize the cascade as a router** (FrugalGPT/RouteLLM shape, already latent in roles/postures): 1.7B = classify/filter/route; 4B = extraction + short grounded summarization + (new) a *constrained few-tool grammar path* so local users get limited tool use honestly; cloud = the agent loop + long-context synthesis. Escalation to cloud is a **visible privacy event**: ledgered, redaction unconditionally after the router.
- **Engineered context budgets**: replace 200k pre-stuffing with just-in-time retrieval over lightweight identifiers (ids/titles → `get_meeting` on demand); compaction of the loop transcript when it crosses a budget; token (not turn) budget for @brain history; most-important-context at prompt start/end (lost-in-the-middle).
- **Robust structured output**: native JSON mode where the backend has it (gateway today, anthropic tools later); one retry-with-error on malformed JSON; scoped grammar constraint for the small local schemas (triples, route decisions — tiny schemas won't overflow context like Bielik did).
- **Hard resource discipline**: token caps + wall-clock timeouts on every live-path generation; a tiny priority queue in front of `brain_rt` (user turn preempts/queues ahead of background scans); in-flight turn dedup.
- **Prompt registry**: one module owning all prompt templates + versions; wake-phrases single-sourced; enables A/B on the eval set.

### L4 — The live brain ("gated incremental summarizer") — effort M, needs real-meeting tuning

- Rebuild the live loop on the production incremental-summarization template:
  1. **Novelty gatekeeper** (deterministic signals + optional 1.7B yes/no; p50 ~ms) decides whether anything happened worth a model call — replaces the fixed every-7-ticks cadence.
  2. **Incremental running bullets**: prefix-prompted local call emits *only new bullets or empty*; the running state lives beside the verbatim tail (rolling-summary + recent-verbatim shape) and replaces the raw 600-char reaction window as the reactions/questions substrate.
  3. **Boundary-timed surfacing**: cards/hints surface at topic shifts/lulls, not mid-utterance (field-study finding); cadence becomes a policy, not a constant.
  4. **Meeting end**: the running bullets become Stage-1 note input (faster finals, less re-reading), and the live brain finally sees far-side text if/when live system-audio transcription lands (flagged, not required for v2).
- Whisper contradiction cards stay deterministic-reconcile (that part is best-practice already); they just read the better substrate.

### L5 — Agents & surfaces (the ClickUp-parity layer) — effort M–L

- **Scheduled briefs** ("every Monday 9am: open action items + stale threads"): generalize digest/proactive into user-defined schedules over local tools, propose-accept for anything write-shaped, zero-egress by default. Low-cadence, high-precision (the 77.5% AI-fatigue survey is the design constraint).
- **MCP client** behind the existing consent + redaction + ledger seam — one integration multiplies source coverage (Linear/Notion/anything) instead of bespoke connectors; per-server consent, loud egress.
- **Verify pass** (✓ confirmed in PROJ-123 / ⧗ conflict) — deterministic compare against live connectors; the headline trust feature; rules-based verification is literally Anthropic's top verification tier.

### Sequencing

| Phase | Content | Size |
|---|---|---|
| **P0 hotfixes** | vault_titles leak, stub-echo guard, token caps + timeouts + turn dedup on live path | S |
| **P1 retrieval + eval** | eval set runs first (baseline numbers!) → topic-segments + contextual augmentation + score-fusion → reranker bake-off | M |
| **P2 memory** | relevance-filtered brief → consolidation job → memory import | S–M |
| **P3 orchestration** | router formalization, JIT context + compaction, prompt registry, structured-output hardening | M |
| **P4 live rebuild** | gatekeeper → incremental bullets → boundary surfacing → bullets-as-Stage-1-input | M |
| **P5 agents** | scheduled briefs → MCP client → verify pass | M–L |

Explicitly rejected for v2: multi-agent Ask (15× tokens, wrong shape for a single-user vault), GraphRAG wholesale (Mem0: graphs add ~2% for QA; keep ours for navigation + community summaries only), agent-self-managed memory (pipeline beats self-management in every benchmark), multi-cloud model routing as a feature (invisible; our posture story is stronger), local-GGUF full agent loop (BFCL 7.8% says no).

---

## Fit with Murmur constraints

- **Local-first**: everything in L1/L2/L4 is zero-egress; extraction-based memory *reduces* egress; the router makes cloud escalation an explicit, ledgered, redacted event. MCP client is new egress → per-server consent like `web.rs`-cloned connectors.
- **Obsidian-native**: consolidation output is `.md` + wikilinks; scheduled briefs export as notes.
- **SQLite-canonical**: every new structure (segments index, scored memory view, rollups, running bullets) is rows in the one store; "write to SQLite, pass back an id" is the artifact pattern.
- **Lock model**: all new reads ride `visibility_clause`; rollups/bullets purge-on-seal like facts; the reranker sees only already-gated candidates. Lock-security review required for L1 (new index tables) and L4 (running-bullets persistence).
- **macOS / honesty**: retrieval + memory + orchestration are fully evaluable headless; live-timing UX and reranker latency need a real Mac; say so in any DoD.

## Recommendation & first step

**Adopt the phased plan; start with P0 + the P1 eval spike in one slice.** The single smallest verifiable step: **run the existing bake-off harness on the real dev vault with a fixed ~20-query set and commit the baseline numbers** (recall@5 for FTS/dense/hybrid; judged faithfulness for 5 Ask answers). It costs a day, produces the first real quality numbers in the project's history, and every subsequent Brain-v2 change gets measured against it — which is precisely the discipline that separates "Anthropic-level" from vibes. In parallel, patch the vault_titles leak and the stub-echo (both S, both user-facing today).

## Open questions

- On-device reranker latency on Apple Silicon — no credible public number; needs local measurement (P1 bake-off).
- mistralrs stable-prefix KV-cache reuse — matters for incremental-bullets local latency; unverified.
- Topic-boundary detection quality on Polish bilingual meetings — needs real recordings.
- Brain² Context Engine internals are vendor claims (no independent teardown); the inferred patterns are pattern-matching.
- Live far-side (system audio) streaming transcription cost — required for the live brain to see the whole meeting; parked pending the perf/OOM headroom work.
- brain2 vs Brain² naming collision — now GA'd and marketed by ClickUp; hard blocker before public copy.

## Sources

**Code (this repo, symbols not line numbers):** `transcribe/live.rs` (tick loop, merge_live_caption, run_assistant_query, run_cascade, assistant_system_prompt, TIER*_SUFFIX), `brain_reactions.rs` (reactions_scan, detect_reactions, cards_from_reconcile), `proactive.rs`, `agent.rs` (run_agentic_loop, ESCALATE_SENTINEL, RESULT_BUDGET), `tools.rs` (AssistantScope, GatedToolExecutor), `reason.rs` + `reason/mistral.rs` (MODEL_CACHE, ram_permits_load, GenOptions), `summarize/{provider,redact,roles,related_context,vault_context,gateway,egress_log}.rs`, `settings/postures.rs`, `embed.rs`, `facts.rs`, `user_memory.rs`, `pipeline.rs`, `enrich.rs`, `orchestrate.rs`; prior docs: `docs/research/2026-07-06-note-and-brain-architecture.md`, `2026-07-04-truth-audit-brain-vs-best-practices.md`, `2026-07-05-competing-with-clickup-brain.md`.

**Anthropic (fetched):** building-effective-agents; effective-context-engineering-for-ai-agents; building-agents-with-the-claude-agent-sdk; built-multi-agent-research-system; writing-tools-for-agents; contextual-retrieval; prompt-caching.

**Academic (fetched):** MemGPT arXiv:2310.08560; Generative Agents arXiv:2304.03442; A-MEM arXiv:2502.12110; Mem0 arXiv:2504.19413; LoCoMo arXiv:2402.17753; LongMemEval arXiv:2410.10813; GraphRAG arXiv:2404.16130; Lost-in-the-Middle arXiv:2307.03172; RULER arXiv:2404.06654; NoLiMa arXiv:2502.05167; Qwen3 arXiv:2505.09388; RouteLLM arXiv:2406.18665; FrugalGPT arXiv:2305.05176; incremental summarization arXiv:2510.06677; proactive wake arXiv:2605.30152; boundary timing arXiv:2601.10253; Goldilocks arXiv:2504.09332; Fission-GRPO (BFCL baselines) arXiv:2601.15625; Weaviate hybrid-fusion blog.

**Competitive (fetched):** clickup.com/brain + /blog/brain-2-launch + help.clickup.com Autopilot/Ambient Answers + zenpilot Brain² review + dupple Brain MAX; Fireflies Live Assist guide; granola.ai/updates; Otter Meeting Agent blog + Forbes + Fast Company; Zoom AI Companion 3.0; MS Learn Teams Copilot; Slack huddle notes; Notion AI meeting notes + tldv review; hedy.ai (Meta/Limitless, Rewind shutdown); tldv lawsuits roundup; XDA + GeekWire on Windows Recall; ClickUp AI-sprawl survey.
