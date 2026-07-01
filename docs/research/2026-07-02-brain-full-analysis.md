<!-- Generated 2026-07-02 via /research (murmur-researcher fan-out, 4 angles: agentic loop, conversation+live memory, data layer, prior art+roadmap). Pricing/funding/version claims = point-in-time. -->
# Research: Full analysis of Murmur's brain — architecture, conversational context, live-meeting answering, data layer, and the roadmap

## TL;DR / Verdict

The brain's **architecture is sound and ahead of the local-first category**: one disciplined agentic tool-use loop, every read visibility-gated, every cloud call consent-gated + redacted, a canonical-SQLite data layer whose FTS5 + vec0 + RRF hybrid shape is exactly the published reference pattern, and a bitemporal facts layer that is a legitimate Graphiti-lite. Keep the split; **do not re-architect**.

The problems are **coverage, memory, and lifecycle seams**, not structure:

1. **The brain has no memory of its own conversations.** All chat history is frontend RAM, the backend is stateless per call, threads evaporate on meeting switch/restart, and there is no `thread_id` anywhere (the known trace cross-attribution bug is a symptom).
2. **Live-meeting awareness is real but much narrower than it claims**: mic-only, unlabeled, greedy captions; a ≤6k-char tail (~5–7 minutes of speech) in the system prompt; tools cannot reach the live buffer; the system prompt falsely promises "who said what".
3. **One lock-model strain found**: `live_transcript` is not cleared on Stop and not purged on seal — a sealed meeting's tail can keep egressing (redacted, consented) into chat prompts until the next recording.
4. **Documents and Brain-page notes are write-only memory on a default install** (no FTS leg, vectors only when the e5 model is present, no reindex backfill) — the Brain page invites adding knowledge the brain cannot recall.
5. **A stale-reasoner-snapshot bug candidate**: consent/provider/backend changes don't reach the already-built `CloudReasoner` until restart.
6. The **RAG bake-off remains the single recorded blocker** for vectors-by-default, related-by-meaning, and connector sequencing.

Recommended order: quick correctness fixes (live-buffer clear/purge, doc-FTS coverage, stale reasoner) → run the bake-off → thread persistence → streaming → user-level memory → proactive brain → Linear connector.

## Co już mamy — how the brain actually works (code-verified, v0.6.3)

### 1. One agentic loop, several non-agentic siblings

There is exactly **one** true agentic loop: `run_agentic_loop` (`src-tauri/src/agent.rs:72`). It powers only the **in-meeting surfaces** — @brain threads and ✨ ask-brain (`ask_assistant_chat`, `commands.rs:671`), the voice/wake card and text composer (`ask_assistant_text`, `commands.rs:612` → `transcribe/live.rs:321`).

- **Prompt-protocol JSON, not native tool-use**: the tool catalog is rendered into the system prompt; each step the model replies `{"tool":…}` or `{"answer":…}`, parsed by a balanced-brace extractor (`reason.rs:325,363`). Works identically on all three providers because it rides `SummarizerProvider.complete`.
- **Budgets**: max `CLOUD_MAX_STEPS = 4` steps (`live.rs:286`), each tool result truncated to 4000 bytes (`agent.rs:67`), exact-repeat tool calls deduped, ≤12 chat messages of history (`CHAT_CONTEXT_TURNS`, `commands.rs:637`).
- **Read-only by construction**: the production executor is `allow_writes:false` (`live.rs:412`); `propose_note` writes to executor scratch only; the FE commits on Accept via `save_manual_notes`. `save_note`/`create_reminder` exist, are tested, and are dormant.
- **Gating is exemplary**: `GatedToolExecutor` (`tools.rs:535`) re-reads the session `unlocked` set from the shared Mutex **on every tool call** (`tools.rs:570-574`), so a mid-loop screen-share auto-relock is honored on the next call. The model only ever emits strings; it cannot reach the DB.
- **Cloud-only**: the loop runs only when `brain_backend == Cloud` (`live.rs:397`). Local GGUF / stub backends take the deterministic floor (`voice_action.rs:296` `rag_answer` — fixed FTS + semantic + dossier + web/calendar fan-out, one synthesis call).

**Non-agentic siblings** (a UX inconsistency users will feel): the **Ask page** (`ask_vault`, `commands.rs:1854`) is a deterministic corpus-pack + one completion — no tools, no web, no trace; the **per-meeting chat** (`chat_meeting`, `commands.rs:1139`) is transcript + history → one completion, with the transcript **head**-truncated at 40k chars (long meetings lose the end, `summarize/chat.rs:8-14`); the **MCP server** exposes 6 gated read tools with no loop (`mcp.rs:233-258`) — the external client is the agent. Net: the brain is smarter mid-meeting than on the dedicated Ask surface.

**Tool inventory** (`tools.rs:90-190`): `search_meetings` (FTS via `search_visible`), `search_semantic` (hybrid FTS∪KNN + doc chunks; flag-gated), `get_meeting`, `list_recent_meetings`, `get_open_commitments`, `get_entity_dossier`, `web_search` (Brave, consent-gated, redacted, the one egress tool), `calendar_lookup` (EventKit, local), `propose_note` (DB-free scratch). All meeting reads pass `visibility_clause`.

**Privacy path**: every loop step → `CloudReasoner.structured` → `make_provider` (fail-closed on `cloud_egress_consented`, `summarize/mod.rs:69-75`) → `RedactingProvider` scrubs **both** system and user prompts (`redact.rs:344-358`). No side channel found; `execute_tool` is provably egress-free.

### 2. Conversational context — FE-owned, RAM-only, no cross-session memory

- **@brain / ✨ ask-brain threads**: history lives only in `MeetingConversationStore` signals (`src/app/core/meeting-conversation.store.ts:67-121`); each turn the FE ships **that thread's own turns** (isolated per thread) to a **stateless backend** ("the FE owns the conversation state" — `commands.rs:625-628`), capped at 12 messages (~6 exchanges, silently front-dropped).
- **Persistence: none as threads.** Only accepted note lines persist (`manual_notes`); `assistant_interactions` rows (command/answer/citations) persist per meeting for display only, purged on seal (`db.rs:280-292,2620`), never re-injected into a prompt. Threads evaporate on meeting switch or restart. **No surface has cross-session conversation memory.**
- **Ask page and detail chat** send **uncapped** full history from the FE each call (`ask.component.ts:721-736`, `vault_chat.rs:24-33`) — unbounded prompt growth in long sessions.
- **No `thread_id` exists anywhere** (grep-clean FE+BE). `EVENT_CHAT_TOOL` payloads carry no thread scope (`events.rs:73-84`), so two simultaneous threads cross-attribute trace chips (self-documented, `meeting-conversation.store.ts:865-871`). Voice results are correctly isolated.

### 3. Live-meeting answering — the real mechanics

During recording, "what is this meeting about?" sees: a flat, unlabeled string of the last ≤6,000 chars (`LIVE_TRANSCRIPT_INJECT_CHARS`, `live.rs:864`) of **mic-stream-only** greedy captions (3s tick, trailing 14s window, same model as batch — default large-v3; `live.rs:23,130-163`), injected into the system prompt, plus ≤6k of typed notes (visibility-gated). Buffer: `AppState.live_transcript` (`state.rs:122`), word-overlap deduped, 16k cap, cleared at the **start** of the next recording.

What it does **not** see, and the sharp edges:

- **No speaker attribution, no timestamps, no system audio** — the `Me`/`Others` split exists only in the batch pipeline at Stop; with headphones the remote side is invisible live. Yet the system prompt tells the model the live transcript can answer "who said what" (`live.rs:961-963`) — any attribution is hallucinated.
- **Tools cannot read the live buffer**, and the in-progress meeting has no persisted segments until Stop — the 6k tail is a hard ceiling on live context. At ~850–1,100 chars/min of speech, 16k ≈ 15–19 min and the injected 6k ≈ **5–7 minutes**; late in a 60-minute meeting the opening is silently gone.
- **Dedup fragility in Polish**: `merge_live_caption` compares words with `eq_ignore_ascii_case` (`live.rs:889-891`) — no Unicode case-folding ("Że" ≠ "że") and punctuation variance truncates overlaps → duplicated text burns the cap faster and feeds the model a stuttering transcript. Magnitude unmeasured.
- **Lock-model strain (the actionable defect)**: nothing clears `live_transcript` on Stop, and lock paths never touch it — a just-sealed meeting's tail stays in RAM and keeps egressing into subsequent chat prompts (redacted + consented, but bypassing `visibility_clause`) until the next recording. Post-Stop threads also get a stale "you are CURRENTLY in a live meeting" prompt while the grounding regime silently flips (typed-notes injection off, persistence off, tools finally on).
- **Local-brain users get zero live awareness** — injection sits entirely inside the Cloud branch (`live.rs:397-441`).

### 4. Data layer — the substrate map and verdict

All substrates are **derived, purgeable indexes** over the one SQLCipher DB (no second copy of content — the right shape). Per-substrate verdict:

| Substrate | State | Verdict |
|---|---|---|
| **FTS5** (3 external-content tables, `unicode61 remove_diacritics 2`, trigger-synced, real BM25; `db.rs:486-510,2937`) | shipped, always-on | **KEEP** — the workhorse |
| **e5 vectors** (`note_chunks` + `vec_chunks vec0(float[384])`, ~800-char chunks, RRF k=60 fusion; `db.rs:442-465,1299`) | shipped plumbing, **dormant by default** (flag off + model absent; `config.rs:234`, `pipeline.rs:609`) | **KEEP, PROMOTE after the bake-off** (per 2026-07-01 brief) |
| **Entity graph** (`entities`/`entity_mentions`, extracted by the cloud provider at Stop; `graph.rs:31`) | shipped, always-on | **KEEP** — cheap 3rd RRF leg + product identity |
| **Facts** (bitemporal, deterministic invalidate-not-delete reconcile — a Graphiti-lite validated by the Zep/Graphiti literature; `facts.rs:114-184`) | shipped, **barely consumed** (dossier only; `entity_dossier` has no FE caller) | **KEEP, but SURFACE it or it's dead weight** |
| **Documents/brain notes** (`documents(kind)`/`doc_chunks`/`doc_vec_chunks`) | shipped, **retrieval hole** | **MERGE into the retrieval plane — highest-priority data fix** |
| **live_transcript** RAM buffer | shipped | **KEEP** (fix lifecycle, above) |

The **document hole**, concretely: ingest stores chunks+vectors **only when the real e5 model is present** (`commands.rs:868-960`; model absent ⇒ zero `doc_chunks` rows), there is **no FTS table over documents**, the only readers are the KNN leg of hybrid Ask and the flag-gated `search_semantic` tool, and `reindex_embeddings_inner` backfills meetings only (`commands.rs:3042-3091`). On a default install an imported document or typed Brain-page note is **unreachable by Ask, the agent, search, and MCP**. Second freshness gap: manual note edits leave stale vectors until the next resummarize (indexing runs only from the Stop pipeline/reindex).

External validation: FTS5 + sqlite-vec + RRF in one SQLite file is exactly the pattern recommended by sqlite-vec's author ([Alex Garcia](https://alexgarcia.xyz/blog/2024/sqlite-vec-hybrid-search/index.html), [Simon Willison](https://simonwillison.net/2024/Oct/4/hybrid-full-text-search-and-vector-search-with-sqlite/)); brute-force vec0 is ample at personal-vault scale (ANN is only alpha upstream — [issue #25](https://github.com/asg017/sqlite-vec/issues/25)); the facts design matches Zep/Graphiti's bitemporal model ([arXiv:2501.13956](https://arxiv.org/abs/2501.13956)); the Letta/MemGPT taxonomy maps cleanly (live_transcript = core memory, gated substrates = archival, the loop = the MemGPT pattern — [Letta](https://www.letta.com/blog/agent-memory/)). **Our lock-gating/purge discipline exceeds every surveyed peer.** What the field does NOT validate: e5-small semantic gain over BM25 for Polish meeting notes at vault scale — that is exactly the unrun bake-off.

## Findings — honest weaknesses (all code-cited by the angle briefs)

1. **Stale reasoner snapshot** (high confidence on code, medium on impact): `state.reasoner` is built once at startup and `CloudReasoner` clones config — including `cloud_egress_consented` and provider — at construction (`state.rs:113-116`, `reason.rs:441-447`). Consent grants / provider switches / backend flips don't reach the loop until restart.
2. **Latency architecture**: each loop step builds a fresh provider; `claude_code` spawns a fresh `claude -p` process per step, plus one extra cloud call per free-form turn just to compute the floor's intent — up to ~6 spawns per @brain question. No caching, no session reuse; the 12k of system-prompt context is re-sent and re-redacted every step.
3. **No answer streaming** — only tool chips stream; deliberately deferred (needs `complete_streaming` + a redaction hold-back buffer so a `⟪NAME_n⟫` placeholder never renders partially).
4. **Brittle non-convergence**: one malformed model reply discards the whole loop's gathered tool work (`agent.rs:97-99,150-151`); no single-step retry; the floor then re-retrieves from scratch. Loop→floor fallback logs at debug only — silent degradation invisible to the user.
5. **Hardcoded budgets everywhere** (4 steps / 4000 bytes / 6k / 16k / 12 turns / 20 hits) — none adaptive to provider or model context size.
6. **Split-brain retrieval UX**: Ask page and detail chat bypass the loop (no web/calendar/dossier/commitments/trace).
7. **Live-buffer lifecycle** (see §3): stale after Stop, not purged on seal, Polish dedup fragility, mic-only blind spot, over-promising prompt.
8. **Document/notes retrieval hole + facts under-consumption + stale vectors on manual edit** (see §4).

## Fit z ograniczeniami Murmur

- **Local-first / redaction**: intact on every checked path; the only egress is `make_provider` (fail-closed consent + redaction on system+user) and the separately-consented web connector. One thing to state plainly: **facts and entity extraction use the cloud provider by default** (consented + redacted) — consistent with the shipped design, but the 2026-06-29 doc's "facts must be local" assumption is not what shipped.
- **Lock model**: stronger than any surveyed peer (purge-on-seal in the seal tx + gated readers per substrate), with the **one RAM-shadow exception**: `live_transcript` survives seal. Fixes here (and any new FTS-over-documents table, thread persistence, or memory export) are mandatory `lock-security-reviewer` gates.
- **SQLite-canonical**: all substrates derived; conversation threads are today the one content class in neither SQLite nor the vault. Thread persistence and user memory should be additive gated tables copying the `assistant_interactions` purge pattern.
- **Provider seam**: streaming = one additive `complete_streaming` trait method; memory-brief injection mirrors the shipped `live_transcript` pattern.
- **CI honesty**: bake-off quality, live dedup magnitude, live-caption cadence on large-v3, streaming feel, diarization, and local tool-calling reliability all need a **real Mac**; headless proves plumbing + gating only.

## Opcje i tradeoffy — prioritized roadmap

**Tier 0 — correctness fixes (S, headless-testable, do first):**

| # | Item | Effort | Why |
|---|---|---|---|
| 0a | **Clear `live_transcript` on Stop + purge on lock/relock** | S | Closes the seal RAM-shadow + the stale "currently live" prompt. RED test: seal folder → next `assistant_system_prompt` input empty. |
| 0b | **Close the document-retrieval hole**: always chunk into `doc_chunks` (embed only when model present), add `fts_doc_chunks` (external-content, same tokenizer, seal-purge like `fts_notes`), add a doc-FTS leg to `search_visible`/flag-off Ask + a 4th RRF list, extend reindex backfill to documents | S | The Brain page's promise actually holds on a fresh install. RED test: import a `.txt` with a unique token on a model-less DB → Ask must find it. Lock-review required. |
| 0c | **Fix the stale reasoner snapshot** (rebuild/Mutex-wrap on config save + consent grant) | S | Removes a "consented but still refused until restart" bug class. RED test first. |
| 0d | Unicode/punctuation-normalized `merge_live_caption` compare + reword the system prompt to stop claiming speaker knowledge | XS–S | Polish live-buffer quality; honesty. |

**Tier 1 — the recorded gate:**

1. **Run the RAG bake-off** (S, half-day, user-run on a real Mac; `docs/RAG-BAKEOFF.md`): ~20–30 PL+EN queries, 4 buckets. Three briefs now block on it; outcome sequences vectors-by-default (+ first-run e5 download + backfill + re-embed-on-edit) and connector timing.

**Tier 2 — memory & feel (dependency order 2→3→4):**

2. **Thread-id + thread persistence** (S–M): additive `thread_id` on `assistant_interactions` + in tool/result event payloads; persist thread turns gated + purged-on-seal; hydrate on return to a meeting. Fixes cross-attribution; threads survive the meeting (the Granola/AskFred durable-chat bar); becomes the episodic-memory substrate.
3. **Token answer streaming with redaction hold-back** (M): `complete_streaming` on the provider seam (`claude -p --output-format stream-json` / Anthropic SSE); RED-first "never a split placeholder, byte-identical restore" test. The live-feel lever.
4. **Cross-meeting user memory — "facts about ME"** (M): extend the shipped bitemporal facts layer with a user-scoped subject + periodic synthesis into an **auditable memory brief** (the Claude-memory pattern), injected like `live_transcript`; provenance-anchored per source meeting, purge-and-regenerate on seal; optionally exported as an owned vault `.md` (lock-review + product call). Also: surface facts generally — wire `entity_dossier` into the FE, inject current facts into Ask/agent grounding — or the facts layer stays dead weight.

**Tier 3 — differentiation:**

5. **Proactive brain** (M, after 1+4): zero-egress in-meeting "you discussed this on Jun 12 →" cards (live tail vs facts + `related_meetings` + open commitments, local reads only) + post-meeting "this updates a fact from 2 weeks ago". Needs a strict relevance threshold + easy mute.
6. **Linear connector, write-first via propose-accept** (M): already specced; sequenced after the bake-off.
7. **Per-speaker diarization — split "Others"** (L, high risk, real-Mac): the loudest unmet demand across Granola/Notion/Hyprnote; feeds facts/commitments quality ("who committed"). Do after 1–4 unless positioning demands sooner.
8. **Local-reasoner re-eval spike** (S, defer ~2 quarters): 2026 consensus says dense ~14B isn't a reliable production tool-caller; cloud stays the reasoner, local stays the floor; re-measure on our GGUFs then.

**Also worth folding into Tier 2 scope**: unify the Ask page onto the agentic loop (kills the split-brain UX; M), cap the uncapped Ask/detail histories (XS), and a single-step retry on malformed loop replies + a visible "floor answer" indicator (S).

## Rekomendacja i pierwszy krok

**Keep the architecture; fix the seams; then build memory.** The immediate, verifiable slice is **Tier 0** — four small, headless-testable, RED-first fixes (live-buffer lifecycle, doc-FTS coverage, stale reasoner, dedup normalization), each independently shippable, 0a+0b requiring lock-security review. **In parallel, run the bake-off on a real Mac** (user task, zero code) — it unblocks the whole vectors/connectors sequencing. First memory feature after that: **thread persistence (item 2)** — smallest slice = additive `thread_id` column + event field + persist-and-hydrate with a RED test that sealed-folder thread turns are purged and invisible.

## Otwarte pytania / czego nie udało się zweryfikować

- **Retrieval quality (PL, names, paraphrase) is unmeasured** — only the bake-off answers whether the semantic leg earns default-on.
- **Real @brain turn latency** with `claude_code` (up to ~6 process spawns) — needs a real-Mac timing spike.
- Whether the **stale-consent scenario bites in practice** (onboarding may restart the app first) — needs a live repro.
- **Polish live-dedup duplication magnitude** and whether large-v3 keeps the 14s-window-per-tick budget on real hardware.
- Whether real users' **facts tables are non-empty** (extraction needs cloud consent + entities present) and how much predicate drift occurs.
- Whether an exported vault `Memory.md` is acceptable under the lock model (cross-meeting artifact with individually-sealable sources) — product + lock-security call.
- Loop reliability on `ollama`/small models when `brain_backend=Cloud` + local provider — untested.
- Competitor figures (Granola redesign dissatisfaction, Fathom ratings, local-LLM tool-calling thresholds) are blog/review-tier, point-in-time mid-2026.

## Sources

**Internal (key file:line):** `src-tauri/src/agent.rs:67-156`; `src-tauri/src/tools.rs:90-342,535-694`; `src-tauri/src/transcribe/live.rs:23-163,286,321-483,861-976`; `src-tauri/src/voice_action.rs:296-551`; `src-tauri/src/reason.rs:257-297,325-523`; `src-tauri/src/commands.rs:612-733,868-960,1139-1176,1854-1911,3042-3091`; `src-tauri/src/summarize/mod.rs:63-131` + `redact.rs:344-358`; `src-tauri/src/storage/db.rs:280-292,442-510,1061-1103,1299,2620,2937`; `src-tauri/src/facts.rs:114-184,242`; `src-tauri/src/state.rs:113-127,122`; `src-tauri/src/mcp.rs:233-345`; `src/app/core/meeting-conversation.store.ts:67-121,335-346,589-607,865-871,942-990`; `src/app/features/ask/ask.component.ts:708-736`; `src-tauri/src/events.rs:73-84`. Prior briefs adopted: `docs/research/2026-07-01-vectors-by-default-and-pure-vector.md`, `docs/research/2026-07-01-mcp-connectors-slack-jira-linear.md`, `docs/research/2026-06-29-murmur-deep-analysis-context-engines.md`, `docs/superpowers/specs/2026-06-30-agentic-assistant-design.md`.

**External:**
1. https://alexgarcia.xyz/blog/2024/sqlite-vec-hybrid-search/index.html — the reference local hybrid pattern (FTS5+vec0+RRF).
2. https://simonwillison.net/2024/Oct/4/hybrid-full-text-search-and-vector-search-with-sqlite/
3. https://github.com/asg017/sqlite-vec/issues/25 — brute-force today, ANN alpha.
4. https://arxiv.org/abs/2501.13956 — Zep temporal knowledge graph (validates `facts.rs`).
5. https://www.emergentmind.com/topics/mem0-system — Mem0's LLM reconcile vs our deterministic exact-key.
6. https://www.letta.com/blog/agent-memory/ — core/recall/archival taxonomy.
7. https://tldv.io/blog/granola-review/ + https://efficient.app/apps/granola — Granola chat-with-meetings, limits.
8. https://www.useluminix.com/reports/industry-analysis/ai-meeting-notes-comparison-granola-vs-otter-vs-fireflies-vs-fathom-2026 — AskFred cross-meeting query.
9. https://www.notion.com/product/ai-meeting-notes + https://tldv.io/blog/notion-ai-meeting-notes-review/ — "perfect meeting memory", no speaker ID.
10. https://hn.algolia.com/api/v1/items/44725306 — Hyprnote HN launch: diarization = #1 blocker in the local-first lane.
11. https://github.com/screenpipe/screenpipe + https://rewind.sh/ — always-on local memory demand; Rewind shutdown.
12. https://www.tomsguide.com/ai/claude-just-unlocked-memory-that-syncs-with-chatgpt-heres-how-it-works — the auditable memory-document pattern.
13. https://insiderllm.com/guides/best-local-llms-mac-2026/ — local tool-calling reliability consensus (defer local reasoner).
