# Murmur AI context and knowledge-system audit

**Date:** 2026-08-13

**Code baseline:** `518d96c1c05c97d2b513e7959555edcd6c3819cb` (`origin/murmur`)

**Scope:** engine/provider routing, retrieval, prompt/context assembly, Brain memory, links, Ask, MCP, dashboards and Living Answers, plus current external approaches.

## Executive verdict

Murmur already has a strong modern knowledge substrate. It combines SQLCipher-canonical data, FTS5/BM25, local multilingual embeddings, entity-neighbour graph scoring, temporal filtering, hierarchical document chunks, bitemporal facts, incremental rollups, bounded tools, and visibility gates. In substance, it is ahead of most meeting-note products and already implements the useful local-first parts of contextual retrieval, temporal memory, and graph-assisted RAG.

It is **not yet maximally efficient at context management**. Candidate discovery is fine-grained, but the deterministic Ask floor often turns those candidates back into whole-note Markdown under static character budgets. The system also loses typed provenance for note/document and derived-dashboard evidence. That prevents precise citations, exact cache dependencies, reliable freshness signals, and evidence-level quality evaluation.

Two concrete resource-bound defects were confirmed. The first was small enough to fix in this change: the explicit on-device `local` connection received the cloud-sized 200,000-character Ask corpus budget because `vault_context::budget_for` recognized only `ollama`. The local sidecar plans around a roughly 4096-token window, so this mismatch could create an oversized prefill and crowd out the actual question and answer. This change makes the vault packer reuse the existing on-device/weak-provider classification and caps `local`, Apple Foundation Models, and Ollama at 4,000 characters. It does **not** claim measured latency, RAM, or answer-quality improvement; those require a real-model benchmark.

The second defect is in durable Ask history: `capped_ask_history` retains the newest 12 turns but does not apply a character budget. Twelve pasted-document-sized turns can therefore enter both the deterministic and agentic prompt, while agent transcript compaction never trims that immutable head. This needs a separate narrow Harness receipt because it changes a different egress-bearing surface and was discovered after this task's owned-path contract was bound.

The highest-value next investment is not another database, GraphRAG service, compression model, or reranker. It is one provider-neutral **evidence-span context compiler** that emits both prompt text and a typed dependency manifest.

## Method and evidence boundary

The audit traced the current code from provider-role resolution through gated retrieval, packing, model dispatch, durable Ask history, links, memory consolidation, MCP, and dashboard rendering. It also compared current primary papers, official engineering material, official product documentation, and open-source implementations.

Confidence labels mean:

- **High:** directly verified in current code or an authoritative primary source.
- **Medium:** the mechanism is verified but its user impact has not been measured on a representative real vault.
- **Low:** a competitor exposes only product behaviour, not engine internals.

No real-Mac local-model latency/RAM run, private-vault quality evaluation, or competitor account test was performed. Historical Murmur evaluation numbers are not reused as current measurements here.

## Current architecture

| Layer | Current implementation | Assessment |
|---|---|---|
| Canonical truth | SQLCipher SQLite owns meetings, notes, documents, chunks, links, entities, facts, interactions, dashboards, and caches. Obsidian, UI, and MCP are projections/readers. | Correct local-first foundation. **High** |
| Provider engine | `summarize::roles::provider_target` resolves role → connection/model/effort. `summarize::make_provider_resolved` owns provider construction, consent, redaction, and egress-ledger admission. Explicit `local` builds `LocalSummarizerProvider` over the killable sidecar and the shared heavy-inference gate. | Strong single policy seam; no reason to split it. **High** |
| Indexing | Meeting notes/transcripts and document chunks are embedded locally; topic chunks add title/date/attendee/fact context. Documents retain leaf chunks, outline summaries, and gated section-parent expansion. | Already overlaps materially with Contextual Retrieval and deterministic hierarchical RAG. **High** |
| Retrieval | `Db::search_hybrid_visible` fuses normalized FTS, KNN, and graph scores at 0.4/0.4/0.2, redistributing weight when a leg is empty. It applies visibility and optional temporal windows. | More substantive than a cosmetic knowledge graph. **High** |
| Reranking | Ask may pointwise-rerank the top 10 candidates with a local reasoner and a 3-second bound, degrading to the original order. | Safely bounded, but value and call cost are not currently demonstrated. **Medium** |
| Context packing | `summarize::vault_context` selects up to 40 candidates, then packs full note Markdown and document snippets into a provider-class character budget. Pinned sources are fairly allocated; active linked neighbours are deduplicated and globally capped at eight. | Correct bounds and gates, but evidence granularity is lost after retrieval. **High** |
| Agentic Ask | The read-only `GatedToolExecutor` exposes allowlisted tools. Ask is capped at six steps, deduplicates repeated calls, bounds gathered output, compacts the loop transcript, and falls back to the deterministic floor on ordinary non-convergence. JIT listing/retrieval remains off by default. | Sensible bounded-agent design; default-off JIT is appropriate until faithfulness is measured. **High** |
| Long-term memory | Entity and user facts are bitemporal: superseded facts close validity instead of being overwritten. Consolidation scores visible facts, produces entity/weekly rollups, and regenerates on source fact-set hash changes with seal-epoch protection. | Close in spirit to temporal knowledge systems such as Graphiti, without a second graph store. **High** |
| Links and graph | Manual, wikilink, companion, semantic, entity, note, document, and meeting relationships are typed DB records. Visible readers gate both endpoints. Links influence navigation and retrieval, not only graph visualization. | Strong and correctly placed in SQLite. **High** |
| Dashboard context | Material tiles resolve through gated readers. Derived board briefs are assembled under a lifecycle guard. Living Answer reads fail closed when their stamped readable-folder set is no longer readable. | Safe but conservative; provenance/freshness is incomplete. **High** |
| Consumption | UI, local MCP, and Obsidian consume the same store. MCP reuses gated tool resolution and revalidates visibility before response admission. | Correct three-surface architecture. **High** |

### End-to-end flow

```text
SQLite/SQLCipher canonical objects
        │
        ├─ FTS5/BM25 ───────────────┐
        ├─ local E5 chunks/KNN ─────┼─ score fusion + temporal filter
        └─ entity/link neighbourhood ┘
                         │
                 optional top-10 rerank
                         │
          current whole-note/snippet packer
               + pinned/link expansion
                         │
          gated deterministic floor or
          bounded agentic read-tool loop
                         │
        provider seam: local or consent +
        redaction + ledger for cloud egress
                         │
             Ask/UI, MCP, Obsidian
```

The weak point is the box labelled “current whole-note/snippet packer”: it returns prompt text plus meeting-shaped source chips, not a complete account of the evidence that actually survived packing.

## Findings

### 1. Confirmed defect: the explicit local Brain received a cloud context budget

At the baseline revision, `summarize::vault_context::budget_for` returned 4,000 characters only for the literal `ollama`; every other connection received 200,000. However:

- `summarize::roles::provider_target` preserves an explicit `local` Ask target;
- `summarize::make_provider_resolved` builds a real `LocalSummarizerProvider` for it;
- `summarize::local::LocalSummarizerProvider::complete` sends the assembled system/user prompt to the local reasoner;
- `summarize::timeline` documents the same local/Apple/Ollama model class as residency-bound and plans local generation around a roughly 4096-token window;
- `summarize::related_context::is_weak_provider` already defines the canonical three-way local class: `local`, Apple Foundation Models, and Ollama.

Therefore the Ask floor could pack about 50 times more vault text for `local` than for Ollama before the prompt reached the same class of constrained on-device engine. This is a wiring defect, not a speculative optimization. **Confidence: high.**

The fix reuses `related_context::is_weak_provider` in `vault_context::budget_for`. Regression coverage binds the full current on-device matrix, a representative cloud provider, and both whole-vault and pinned construction with a long multibyte corpus. Cloud budget, egress classification, redaction, consent, ledger behaviour, retrieval ranking, visibility gates, and link caps are unchanged.

### 2. Confirmed follow-up defect: durable Ask history has only a turn-count cap

The ordinary in-meeting chat path applies both `CHAT_CONTEXT_TURNS` and `CHAT_HISTORY_CHAR_BUDGET`. Whole-vault Ask instead passes its durable history through `capped_ask_history`, which takes only the newest 12 `ChatTurn`s. Both `vault_chat::render_conversation` and the deterministic floor render those contents in full. On the agentic route, `LoopTranscript` compaction can remove old tool-result blocks but deliberately never trims its initial head, which already contains the full rendered history.

As a result, 12 legal history records can contain hundreds of thousands of characters, making the 32,000-character loop-transcript target ineffective and increasing cloud egress, local context overflow risk, latency, and failure probability. Existing tests prove only that turn 13 is dropped; they do not exercise oversized content. **Confidence: high.**

The follow-up should enforce a strict Ask-specific character budget after the 12-turn cap, retain the newest complete turns first, preserve the current question (which is a separate argument), and deterministically tail-truncate only a single oversized newest prior turn. A multibyte Polish/emoji regression must prove both the bound and newest-context retention.

### 3. The main structural inefficiency is evidence packaging, not candidate discovery

Hybrid retrieval operates on chunks and independent score legs, yet `vault_context::pack_meetings` reads each chosen meeting's full latest note and takes as much as fits. A long first note can occupy most of the budget, while several shorter independent pieces of evidence disappear. Fine-grained retrieval effort is therefore partly discarded at the last stage.

Static provider-class character limits also do not reserve explicit space for:

- system and safety instructions;
- durable Ask history;
- pinned/board context;
- agent tool results;
- the current question;
- the completion.

The agentic loop has its own 32,000-character transcript and 64,000-character gathered-result bounds. In-meeting chat has a 64,000-character history bound. Durable Ask uses a 12-turn count cap but no equivalent per-turn character budget in `capped_ask_history`. These independently reasonable limits do not form a single end-to-end context allocation.

Long contexts are not automatically better. [Lost in the Middle](https://arxiv.org/abs/2307.03172) shows position sensitivity and degradation when relevant evidence is surrounded by distractors. This supports reducing irrelevant prompt material, but the size of the Murmur-specific quality/latency effect remains unmeasured. **Confidence: high for the mismatch; medium for product impact.**

### 4. Provenance is materially coarser than retrieval

`VaultSource` is meeting-shaped. A pinned standalone note/document can be included in the provider prompt while the returned source list remains empty; linked note/document neighbours have the same problem. The deterministic floor also returns no model-independent citation strings for these contributors. Durable Ask then persists that incomplete evidence record, and Angular can only render the sources it received.

Document chunks and board-derived rows can therefore influence an answer without one stable structured identity that says:

- what object and span contributed;
- which folder made it readable;
- which retrieval leg/rank selected it;
- how much was packed and whether it was truncated;
- which source revision or content hash the answer depends on.

This is an answer-integrity and auditability gap, not a confirmed lock bypass. [ALCE](https://arxiv.org/abs/2305.14627) separates citation correctness and completeness from answer quality, and Microsoft's [VeriTrail](https://www.microsoft.com/en-us/research/blog/veritrail-detecting-hallucination-and-tracing-provenance-in-multi-step-ai-workflows/) similarly treats provenance as stable evidence identities and transformations rather than model-formatted titles. **Confidence: high.**

### 5. Dashboard safety is conservative; freshness and snapshot identity are incomplete

Current material and derived dashboard reads are gated. `seal_epoch`/dispatch admission rejects relock changes during inference, and Living Answer reads withhold cached text when the recorded readable-folder set is no longer readable. No direct sealed-content bypass was found.

The conservative cache stamp records every currently readable folder because the actual contributor set is unavailable. This has three consequences:

1. Locking an unrelated folder can withhold an otherwise valid answer.
2. A very large folder set can approach the 8 KiB tile-config limit.
3. A source edit/deletion/reorder can leave an answer semantically stale while folder readability remains unchanged.

There is also no monotonic dashboard composition revision spanning “resolve material sources + render derived brief + generate/persist answer”. The FE and backend resolve parts of that context in separate operations, so composition reorder, X→Y→X ABA, or delete/recreate changes are not represented by one stable witness. This is scope/freshness risk, not evidence of a current visibility bypass.

The redesigned dashboard UI currently does not call `set_dashboard_answer`, and an E2E test explicitly asserts no persistence call, so the large-folder size cliff is dormant for newly generated Board Ask answers. Existing cached tiles are still read. Do not narrow the folder stamp until derived-board dependencies are complete; doing so prematurely could turn a conservative false-withhold into a sealed-derived cache leak. **Confidence: high.**

### 6. The memory substrate is stronger than the product exposes

Bitemporal fact validity, source-meeting anchors, fact-set-hash rollups, entity/link retrieval, and as-of/diff APIs put Murmur closer to Graphiti's temporal model than to ordinary meeting-summary search. The missing step is not another temporal store. It is making Ask and proactive surfaces visibly answer questions such as:

- What is current?
- What changed, when, and from which source?
- Which fact was contradicted or superseded?
- Is this dashboard answer fresh against the exact evidence revision?

Evaluation currently focuses substantially on meeting-ID retrieval metrics such as recall@k, nDCG, and MRR. It does not yet prove evidence-span coverage, temporal answer correctness, contradiction handling, citation completeness, or robustness to evidence position. **Confidence: high.**

### 7. Plausible optimizations exist, but they are not yet justified bottlenecks

- The optional pointwise reranker can make up to ten serial local-model judgments. It is bounded and fail-soft, but no current trace proves that it improves final answers enough to repay latency.
- Regular meeting chunk re-indexing replaces embeddings, while topic indexing can skip unchanged hashes. Incremental reuse might help edit-heavy vaults, but clean replacement also protects against stale vectors and seal races.
- Learned prompt compression, ColBERT-style late interaction, GraphRAG community summaries, and inference KV caching all have credible research results, but each adds model residency, storage, tuning, or provider coupling.

Measure the final evidence packaging seam first. Optimizing upstream machinery before knowing whether packaging dominates would add complexity without a demonstrated user win. **Confidence: medium.**

## External comparison

| Approach | What is useful for Murmur | What not to copy |
|---|---|---|
| [Granola Chat and Briefs](https://docs.granola.ai/help-center/taking-notes/pre-meeting-briefs) | Situated, small pre-meeting context; scoped chat; visible source/step trail. | Its cloud data boundary and undisclosed internal ranking are not architectural evidence for a local-first engine. |
| [Notion Enterprise Search](https://www.notion.com/help/enterprise-search-security-and-privacy-practices) | Query-time permission checks, explicit citations, and public freshness/deletion expectations. | A cloud connector → external embedding/vector-service pipeline would weaken Murmur's local-first posture. |
| [ClickUp Brain](https://help.clickup.com/hc/en-us/articles/20658787666071-Collaborate-with-ClickUp-Brain-AI-from-anywhere-in-your-Workspace) | Operational AI cards, standups, stale/stuck work, and scoped workspace/app context. | Credit-metered ambient automation and opaque memory/freshness semantics. |
| [Mem Heads Up](https://help.mem.ai/features/heads-up) and [Recall](https://feedback.getrecall.ai/changelog) | Related context at the active note/browser location; exact mention jumps and paths. | A proprietary parallel card/graph truth store. |
| [Obsidian](https://obsidian.md/about) + [Smart Connections](https://community.obsidian.md/plugins/smart-connections) | Owned Markdown, explicit links, and local context-at-the-cursor. | Similarity suggestions should not silently become durable facts. |
| [Graphiti](https://github.com/getzep/graphiti) / Zep | Episodes as ground truth, validity windows, fact invalidation, lineage, hybrid+graph retrieval. | A second mutable graph database and LLM-heavy ingestion would duplicate Murmur's facts, graph, and SQLite authority. |
| [Letta context hierarchy](https://docs.letta.com/guides/core-concepts/memory/context-hierarchy) | Explicit hot/warm/cold memory tiers and small always-visible state. | Always-visible blocks spend tokens every turn and do not supply temporal provenance by themselves. |
| [Anthropic Contextual Retrieval](https://www.anthropic.com/engineering/contextual-retrieval) | Contextual chunks, BM25+embedding fusion, reranking. Murmur already implements most of this pattern locally. | Vendor-reported gains cannot be projected onto Murmur without its own corpus. |
| [RAPTOR](https://arxiv.org/abs/2401.18059) | Retrieve at multiple abstraction levels. Murmur already has deterministic document parents/outlines and memory rollups. | A second summary tree before proving the current hierarchy misses evidence. |
| [Microsoft GraphRAG](https://www.microsoft.com/en-us/research/publication/from-local-to-global-a-graph-rag-approach-to-query-focused-summarization/) | Community/global questions are useful evaluation cases. | Its [official repository](https://github.com/microsoft/graphrag) warns about indexing cost and corpus-specific prompt tuning; it is not a free upgrade. |
| [Adaptive-RAG](https://aclanthology.org/2024.naacl-long.389/) | Route simple vs multi-step questions to different retrieval depth. | Enable broader JIT retrieval only after packaging and faithfulness have an evaluation gate. |
| [LongMemEval](https://arxiv.org/abs/2410.10813) | Temporal updates, abstention, session decomposition, and long-term memory cases. | Another memory store; Murmur already has the needed temporal substrate. |
| [LongLLMLingua](https://aclanthology.org/2024.acl-long.91/) / [LLMLingua-2](https://aclanthology.org/2024.findings-acl.57/) | Token reduction is a useful measured outcome. | An additional compression model before deterministic span selection, especially on memory-constrained Macs. |
| [ColBERTv2](https://arxiv.org/abs/2112.01488) | More expressive late interaction if single-vector retrieval is proven inadequate. | Multi-vector storage/query cost without a demonstrated recall miss. |
| [RAGCache](https://arxiv.org/abs/2404.12457) | Retrieval-aware prefix/KV reuse where the inference engine is controlled. | It is not portable across Murmur's external CLI/API provider seam today. |

The cross-product pattern is clear: the best systems are tiered, source-aware, permission-aware, and situated. The defensible Murmur advantage is achieving that while keeping SQLite/Obsidian ownership and local inference, not matching cloud products by maximizing prompt size.

## Recommended architecture

### Priority 0 — land the local budget correction

This audit's code change is intentionally narrow:

- reuse one canonical on-device classification;
- cap local/Apple/Ollama Ask corpus text at 4,000 characters;
- retain 200,000 for non-weak/cloud-capable identifiers;
- prove whole-vault and pinned packers obey it;
- change no retrieval, prompt, lock, provider, or egress semantics.

### Priority 0b — strictly bound durable Ask history

In a separate egress-reviewed change:

- apply a character budget after the 12-turn cap;
- keep the newest complete prior turns that fit;
- if the newest prior turn alone exceeds the budget, retain a marked tail within the strict bound;
- never alter or drop the separately supplied current question;
- bind both deterministic-floor and agentic rendering through their shared `vault_chat::render_conversation` seam.

### Priority 1 — one typed evidence-span context compiler

Keep the existing retrieval algorithms as the control. Replace the final packaging API incrementally with an internal structure similar to:

```rust
struct PackedContext {
    text: String,
    evidence: Vec<PackedEvidence>,
    budget: ContextBudget,
    truncated: bool,
}

struct PackedEvidence {
    kind: EvidenceKind,
    canonical_id: String,
    span_id: Option<String>,
    folder_id: Option<String>,
    revision_or_hash: String,
    retrieval_leg: RetrievalLeg,
    rank: usize,
    chars_or_estimated_tokens: usize,
    truncated: bool,
}
```

Requirements:

1. Select topic/note/document/fact spans across sources rather than greedily copying whole meetings.
2. Give each high-ranked source a deterministic minimum share and per-source maximum before spending remainder.
3. Preserve prompt text byte-for-byte behind a control flag until evaluation shows a win.
4. Reserve model-aware space for instructions, history, pinned/board context, tool results, question, and completion; degrade conservatively to characters when a provider exposes no tokenizer/window metadata.
5. Record every actual contributor, including linked neighbours, document parents, facts, org items, and board-derived rows.
6. Keep the manifest local. Only already-approved/redacted prompt text crosses the provider seam.
7. Missing provenance fails closed for cache narrowing.

This single abstraction unlocks precise source chips, complete citations, truncation UI, cost/context telemetry, and safe dashboard dependencies without creating a second source of truth.

### Priority 2 — make dashboards atomic and freshness-aware

After complete provenance exists:

1. Resolve material sources and derived board context in one backend snapshot operation.
2. Add a monotonic dashboard composition revision that increments on add/update/delete/reorder.
3. Capture and revalidate the revision around model dispatch/persistence; do not use a content hash alone as the only ABA witness.
4. Store a dependency digest over exact evidence identities plus source revisions/content hashes.
5. Treat “fresh”, “outdated: N sources changed”, and “withheld: source locked” as distinct states.
6. Only then replace the broad all-readable-folders cache stamp with the complete contributor-folder union.

The ideal UI is situated and compact: “Based on 7 sources · 2 changed · Refresh”, with source jumps, rather than an opaque timeless answer tile.

### Priority 3 — evaluate the final answer path, not only retrieval IDs

Use a fixed private PL/EN vault and keep whole-note packing as the control. Measure:

- evidence-span recall@5 and nDCG;
- answer correctness and calibrated abstention;
- temporal update/contradiction/as-of questions inspired by LongMemEval;
- citation precision, completeness, and source-kind coverage;
- robustness to shuffling evidence position;
- prompt input tokens/characters, TTFT, total latency, peak RAM, and local-model call count;
- contributor lock/relock, unrelated-folder lock, source edit/delete, reorder, X→Y→X, and delete/recreate ABA;
- cloud and the actual local Qwen route separately, with served-model identity and timings attested.

A reasonable decision gate is non-inferior answer/evidence quality, no citation/abstention regression, zero visibility/cache regressions, and at least 30% median prompt reduction on long corpus-floor queries. The 30% value is a proposed go/no-go threshold, not a forecast.

### Priority 4 — optimize retrieval only after traces identify the bottleneck

Then consider, in order:

1. remove or batch the pointwise reranker if it costs latency without quality lift;
2. reuse unchanged meeting chunk embeddings if a seal-safe, stale-vector-free design proves worthwhile;
3. enable adaptive/JIT retrieval for query classes where it wins faithfulness/cost;
4. evaluate learned compression or late interaction only if deterministic span selection still misses the target.

Do not adopt Graphiti, Letta, Neo4j/FalkorDB, or Microsoft GraphRAG as a new runtime authority. They are useful design references; Murmur already owns the important primitives in a safer local canonical store.

## Required oracles for the next slice

- A packed standalone note/document and linked neighbour each produce a typed evidence ref and visible citation.
- Material truncated away is not reported as fully packed.
- Every prompt contributor has a folder/revision/hash or is explicitly non-folder-scoped.
- A sealed-not-unlocked contributor never appears in text, refs, titles, cache, or citations.
- A board-brief-only contributor from folder X is visible while X is unlocked and withheld after relock.
- Locking unrelated folder Y does not invalidate a cache that depends only on X.
- Source mutation/deletion, tile reorder, X→Y→X, and delete/recreate reject stale persistence.
- A 208+ unrelated-folder fixture no longer exhausts the tile config after safe dependency narrowing.
- The control prompt remains byte-identical when the new compiler flag is off.
- Real-corpus evaluation reports evidence coverage and final answer/citation quality, not only meeting-ID recall.

## Limitations and open questions

- The corrected 4,000-character local bound is a conservative class-level guard, not full per-model token budgeting.
- The verification host currently has Codex CLI 0.147.0 in Homebrew and the production-verified 0.146.0 binary in `~/.local/bin`. Murmur correctly fails closed on the unverified minor. Harness checks replace `HOME`, and production binary discovery deliberately ignores ambient `PATH`, so the task-private Harness home exposes a symlink to the user-owned 0.146.0 binary; the runtime tests still canonicalize, ownership-check, permission-check, version-check, and identity-pin that target. This is compatibility evidence for 0.146.0, not evidence that 0.147.0 is unsafe or incompatible.
- No live Qwen/Apple/Ollama run was performed, so no TTFT, total latency, RAM, or quality gain is claimed for the fix.
- The relative cost of retrieval, serial reranking, indexing, and local generation is still unprofiled on a representative vault.
- It remains unmeasured whether whole-note packing causes more answer errors than token waste in the user's real data.
- Consumer competitors generally do not publish chunking, ranking, cache keys, latency SLOs, or complete citation semantics; their comparison rows describe verified product behaviour, not reverse-engineered internals.
- Current public-source facts and prices can change; pricing was not used to make the architectural recommendation.

## Primary external sources

1. [Lost in the Middle](https://arxiv.org/abs/2307.03172)
2. [RAPTOR](https://arxiv.org/abs/2401.18059)
3. [Anthropic Contextual Retrieval](https://www.anthropic.com/engineering/contextual-retrieval)
4. [Microsoft GraphRAG paper](https://www.microsoft.com/en-us/research/publication/from-local-to-global-a-graph-rag-approach-to-query-focused-summarization/) and [repository](https://github.com/microsoft/graphrag)
5. [Adaptive-RAG](https://aclanthology.org/2024.naacl-long.389/)
6. [LongLLMLingua](https://aclanthology.org/2024.acl-long.91/) and [LLMLingua-2](https://aclanthology.org/2024.findings-acl.57/)
7. [LongMemEval](https://arxiv.org/abs/2410.10813)
8. [ColBERTv2](https://arxiv.org/abs/2112.01488)
9. [RAGCache](https://arxiv.org/abs/2404.12457)
10. [ALCE](https://arxiv.org/abs/2305.14627)
11. [Microsoft VeriTrail](https://www.microsoft.com/en-us/research/blog/veritrail-detecting-hallucination-and-tracing-provenance-in-multi-step-ai-workflows/)
12. [Graphiti](https://github.com/getzep/graphiti)
13. [Letta memory hierarchy](https://docs.letta.com/guides/core-concepts/memory/context-hierarchy)
14. [Granola Chat](https://docs.granola.ai/help-center/getting-more-from-your-notes/chatting-with-your-meetings), [Briefs](https://docs.granola.ai/help-center/taking-notes/pre-meeting-briefs), and [People/Companies](https://docs.granola.ai/help-center/people-and-companies)
15. [Notion Enterprise Search architecture](https://www.notion.com/help/enterprise-search-security-and-privacy-practices)
16. [ClickUp Connected Search](https://help.clickup.com/hc/en-us/articles/14642390285463-Connected-Search) and [Brain surfaces](https://help.clickup.com/hc/en-us/articles/20658787666071-Collaborate-with-ClickUp-Brain-AI-from-anywhere-in-your-Workspace)
17. [Mem Search](https://help.mem.ai/features/search) and [Heads Up](https://help.mem.ai/features/heads-up)
18. [Obsidian principles](https://obsidian.md/about) and [Smart Connections](https://community.obsidian.md/plugins/smart-connections)
