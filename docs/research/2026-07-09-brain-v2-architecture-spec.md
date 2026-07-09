<!-- Generated 2026-07-09 via 3 parallel code-architect blueprints (L1+L2 / P0+L3 / L4+L5), synthesized. Companion to 2026-07-09-brain-v2-architecture.md (the research + problem analysis). Cite-by-symbol; line anchors drift. -->
# Brain v2 — Implementation Architecture Spec (decision-ready)

**Status:** architect-designed, grounded against the 0.8.0 tree, not yet implemented. The research grounding, problem list, and best-practice citations live in `2026-07-09-brain-v2-architecture.md`. This doc is the build plan: data model, seams, phases, gates, and the consolidated open-decision list.

**Design stance:** workflow-first, agent-when-needed; measured, budgeted, gated. The substrate (SQLite canonical, lock gating, provider seam + redaction + ledger, structural tiering, two-stage notes) is untouched. Everything below is **additive** — new tables via `CREATE TABLE IF NOT EXISTS` / `add_column_if_missing`, new modules, extended signatures; zero destructive migration.

---

## 0. Module map (new + touched)

**New modules (all `src-tauri/src/`):** `router.rs`, `prompts.rs`, `rerank.rs`, `memory.rs`, `summarize/temporal.rs`, `transcribe/bullets.rs`, `brief_runner.rs`, `connectors/mcp_client.rs`, `connectors/mcp.rs`, `eval/fixtures/rag-bakeoff-sample.json`, `eval/results/` (artifact dir).

**New tables (all additive):** `topic_chunks` + `topic_vec_chunks` + `fts_topic_chunks`, `note_chunks.aug_text` (column), `fts_user_facts`, `memory_scores`, `memory_rollups`, `live_bullets`, `brief_schedules` + `brief_runs`, `mcp_servers`.

**New AppState fields:** `in_flight_turns: Mutex<HashMap<String,u32>>`, `user_turn_in_progress: AtomicBool`, `live_bullets: Mutex<String>`, `live_bullets_tracker: Mutex<BulletsTracker>`, `verify_cache: Mutex<HashMap<String, Vec<VerifyFinding>>>`.

**New config keys (all `#[serde(default)]`):** `brain_heavy_grammar_enabled` (false), `ask_jit_retrieval` (false→true after eval), `loop_transcript_compaction` (true), `live_bullets_enabled` (true-if-model-present).

---

## P0 — Hotfixes (one PR, ship first)

### P0.1 `vault_titles` leak — corrected root cause
The NER title filter in `redact.rs::summarize_with_meta` **exists and works** — but only when the NER model is downloaded. On installs without it, `NoopNameRedactor` returns every title unchanged (`name_hits` always empty) so `[[Anna Kowalska]].md` egresses. **Fix:** a conservative syntactic fallback active only when `!ner_model_present()`: `title_looks_like_person_name()` — 2–4 Title-Case words, no digits/acronyms, not on a PL+EN `COMMON_TITLE_WORDS` blocklist (Meeting/Notes/Sync/Spotkanie/Notatki/Przegląd/…). A false positive drops a wikilink target, never content. **RED test:** `person_name_title_is_dropped_when_no_ner_model_present` (EchoProvider captures what reaches the "cloud").

### P0.2 Stub-echo guard — corrected path
The echo reaches users on the **floor** path (`run_informational` → `voice_action` → `reasoner.reason()`), not the cascade (local-only config already skips the cascade via `is_reasoner_only()`). **Fix:** the canonical `reasoner.id() == "stub"` guard (same as `orchestrate.rs`/`user_memory.rs`) right after reasoner resolution in `run_informational` (+ audit `ask_vault` floor), returning a graceful "The on-device brain model is not downloaded yet — enable it in Settings → Brain & AI." **RED test:** stub config → answer contains no `[stub-reason]`.

### P0.3 Live-path resource discipline
- **Token caps:** `run_agentic_loop` gains an `opts: GenOptions` param and calls `structured_with`; new presets `GenOptions::live_answer()` (1024) and `ask_answer()` (2048). Effective today on the local GGUF path (the only truly unbounded one); a best-effort hint for cloud.
- **Wall-clock timeout:** `MistralReasoner::reason_with_timeout` — the blocking call runs on a spawned thread, caller waits `recv_timeout(GENERATION_TIMEOUT = 30s)`, on expiry returns `AppError::Unavailable` and **leaks the task** (mistralrs generations are uncancellable; drop-leak issues #723/#865 forbid dropping the model). The cascade already handles `Err` by tiering down / flooring.
- **In-flight dedup:** `AppState.in_flight_turns` per-meeting counter; a second concurrent turn for the same meeting is **dropped with a log** (not queued — no stale-answer backlog).
- **Priority:** `AppState.user_turn_in_progress: AtomicBool`; the reactions worker defers (returns empty) while a user turn is in flight. Approximate but correct — reactions are best-effort.

---

## L1 — Indexing & retrieval

### L1.1 Topic-segment indexing
Pure `embed::segment_topics(&[Segment]) -> Vec<TopicSegment>` — deterministic boundaries from three signals (any one fires): **lull ≥ 30s**, **speaker-flip after a ≥5-segment run**, **Jaccard lexical shift < 0.15 over a 6-turn window**; segments < 60s merge forward (over-segmentation preferred over under-). Stored in `topic_chunks` (+ `topic_vec_chunks` vec0 + `fts_topic_chunks` external-content FTS5 with triggers). **Purge-on-seal:** added to `purge_chunks_tx` — the single choke point covers every seal path. **Backfill:** `backfill_topic_chunks_idempotent()` at startup on `spawn_blocking`, batches of 20, content-hash idempotent.

### L1.2 Deterministic contextual augmentation
`embed::augment_chunk_text(title, date, attendees≤5, active_facts≤8, raw)` → `"<title> | <date> | <attendees> | <facts>\n<raw>"`, stored as `aug_text` (topic chunks + new `note_chunks.aug_text` column) and indexed in BOTH FTS and `embed_passage` legs (raw `text` kept for snippet display). Attendees/facts come from `visibility_clause`-gated readers; sealed-not-unlocked ⇒ empty header (no PII in index rows that outlive a seal — and the rows purge on seal anyway). This is Anthropic's contextual-retrieval mechanism at zero LLM cost (our entity graph + facts make the situating context templatable).

### L1.3 Score fusion (RRF stays as fallback)
`embed::score_fuse(fts, knn, graph)` — per-leg min-max normalization (KNN distance inverted via `1/(1+d)`), weighted blend `0.4/0.4/0.2` (named consts, calibrated by the eval gate). `search_hybrid_visible` uses it when raw scores are available; falls back to `rrf_fuse` otherwise. `fuse_doc_hits` unchanged.

### L1.4 Reranker seam (Ask-only)
`rerank.rs`: `trait Reranker { fn rerank(query, candidates, timeout_ms) -> Vec<String> }` — **degrades to input order on any failure/timeout, never errors**. Impl 1: `PromptedReranker` — pointwise yes/no relevance on the already-resident Qwen3-1.7B (`max_tokens: 32`), deadline-checked between candidates (`RERANK_TIMEOUT_MS = 3000`). Impl 2: `StubReranker`. Future `BgeReranker` (bge-reranker-v2-m3 via candle, `ner_deberta.rs` load pattern) sits behind the same trait — decision deferred to the measured bake-off. Wired in `vault_context.rs` after top-k, before `pack_meetings`; reorders already-gated candidates only (no new read path).

### L1.5 Time-aware query expansion
`summarize/temporal.rs::parse_temporal_constraint(query, today) -> Option<(from, to)>` — pure PL+EN regex parser (last week/zeszłym tygodniu, this month, yesterday/wczoraj, last N days, ISO dates). Threads `date_filter: Option<(String,String)>` into `search_hybrid_visible` (`AND m.started_at >= ? AND < ?` on all three legs). Original query text is NOT stripped — BM25 tolerates the extra tokens. **RED test:** date-filtered search excludes an out-of-window meeting.

### L1.6 Eval gate (the discipline that makes it "Anthropic-level")
- Commit `eval/fixtures/rag-bakeoff-sample.json` — 20 queries, 4 categories (entity-anchored / paraphrase / cross-lingual PL↔EN / temporal), `expected_meeting_ids` filled by the user against the real dev vault (WAL-copy + dev DEK per `docs/RAG-BAKEOFF.md`; the harness `run_bakeoff_over_real_db_from_env` already exists, env-driven: `MURMUR_BAKEOFF_DB/DEK/SET/K`, new `MURMUR_BAKEOFF_OUT` writes the artifact).
- Committed artifact `eval/results/rag-bakeoff-latest.md`: date, commit sha, config consts, recall@5/nDCG@5/MRR per mode (fts / semantic / hybrid / hybrid+rerank — new `RetrievalMode::Reranked`).
- **Merge rule (manual, adversarial-verifier-checked, not CI):** any retrieval-touching PR updates the artifact and hybrid recall@5 ≥ baseline. CI can't run it (needs model + real DB) — `scripts/ci.sh` gets a comment saying so.

## L2 — Memory

### L2.1 Consolidation/reflection job
`memory.rs` + hourly `tokio::spawn` from `lib.rs` setup (never holds the DB lock across an LLM call; `tracing::warn` on error, never exits). Deterministic scoring per the generative-agents recipe: `composite = 0.4·recency(0.995^hours) + 0.4·importance/10 + 0.2·relevance` into `memory_scores` (FK `ON DELETE CASCADE` to `user_facts` ⇒ purge-on-seal is transitive). Reflection: entity groups (≥3 facts or importance ≥7) → light-reasoner synthesis (256 tokens) → `memory_rollups` (`entity:<id>` / `weekly:<YYYY-WNN>`) → exported as `.md` to `<vault>/brain/memory/`. Rollups are cross-meeting synthesis: **not** purged on seal but **regenerated** next run from still-visible facts only (the job reads via `list_user_facts_visible`).

### L2.2 Relevance-filtered memory brief
New `fts_user_facts` (external-content FTS over `subject predicate: object`) + gated `Db::search_user_facts_visible(query, k=8, unlocked)`. `build_memory_brief(db, query, unlocked)`: query-empty (note-gen path) or FTS-empty ⇒ fall back to today's all-facts `synthesize_brief` — behavior-preserving fallback, `MEMORY_BRIEF_MAX_CHARS` unchanged.

### L2.3 Memory import (ClickUp parity, S)
`import_memories(text) -> usize` IPC command: paste ChatGPT/Claude memory export → `extract_imported_memories` (reuses `extract_user_fact_candidates` machinery, stub ⇒ empty) → `reconcile_facts` dedup → anchored to a **synthetic `Memory Import` meeting row** (no folder ⇒ visible via the existing INNER JOIN; deleting that meeting undoes the whole import — no change to the fail-closed NULL-meeting_id rule). Minimal FE: `memory-import` component inside the existing brain-memory audit view. Zero egress.

## L3 — Reasoning & orchestration

- **`router.rs`:** explicit, testable `route(RouterInput) -> RouteDecision { DeterministicFloor | LocalLight | LocalHeavy | CloudAgentic{connection} }` over the existing roles/postures data (no new struct in the dispatch path initially — additive explicitness). `QueryClass` (Recall/Synthesis/External/Unknown) from the 1.7B or keywords.
- **Escalation = ledgered event:** cascade tier escalation writes a content-free `egress_log` row with `call_kind: "escalation"` — visible in the privacy receipt.
- **Constrained grammar for tiny local schemas:** `GenOptions.use_grammar_constraint` — `MistralReasoner` uses mistralrs `Constraint::JsonSchema` only for schemas < 512 bytes (the Bielik overflow was a huge schema; a 3-key enum is not that), graceful fallback to schema-in-prompt; gated by `brain_heavy_grammar_enabled` (default false until real-Mac tested).
- **JIT retrieval for ask_vault** (behind `ask_jit_retrieval`): replace the 200k-char corpus pre-stuff with a ~2.4k-char meeting **listing** (id | title | date × 30) + a new `get_meeting` tool (**gated by `meeting_is_unlocked` — lock-security review required**) returning title + note + first 4k transcript chars. The non-agentic floor keeps its packed corpus (small-model efficiency wins there).
- **Loop compaction:** deterministic `compact_transcript` at `TRANSCRIPT_BUDGET = 32k` chars (~8k tokens — inside small-model effective context): keep user request + last 2 tool results verbatim + `[N earlier results omitted]`.
- **History budget:** `trim_history_to_budget` (64k chars) in `ask_assistant_chat` — token-ish budget replaces the turn-only cap, trims oldest-first.
- **Structured-output hardening:** one retry-with-error-appended on malformed JSON in `run_agentic_loop`; `SummarizerProvider::supports_native_json()` capability flag (gateway ⇒ true) so `CloudReasoner` can pick native JSON mode when available.
- **`prompts.rs`:** all templates + `PROMPT_VERSION` + single-sourced wake phrases; migrated incrementally (live.rs/agent.rs/vault_chat.rs first) via re-exports, zero behavior change; eval artifacts stamp the version.

## L4 — The live incremental brain

- **Novelty gatekeeper** (`NoveltyState`, pure, on the tick thread): fires on ≥120 new chars / any `?` / entity hit (via a 60-tick-cached entity list shared with `reactions_scan`) / 42s lull; hard floor 15s. Replaces `% REACTIONS_SCAN_EVERY == 0`. The optional 1.7B yes/no confirm is explicitly deferred (Metal-contention risk on the tick thread).
- **Incremental bullets** (`transcribe/bullets.rs::update_bullets`): prefix-prompted light-reasoner call (previous bullets + delta → ≤3 new bullets or `NOTHING`; 200 tokens, temp 0.1), runs on the reactions worker behind `reactions_busy`. State: `AppState.live_bullets` (RAM, 4k cap) + additive `live_bullets` table for crash recovery — **blanked on seal like `assistant_interactions`**, cleared at Stop/relock; reads gated by `meeting_is_unlocked`.
- **New substrate:** reactions read `bullets + last-300-chars verbatim` instead of the raw 600-char tail; the live inject for questions becomes `2k bullets + 2k verbatim` (tighter than today's 6k — better for small models). **At Stop, bullets become a Stage-1 input:** new `SummarizeRequest.live_bullets` field, rendered as a labeled section before the transcript (rides `RedactingProvider` like everything else).
- **Boundary-timed surfacing:** whisper/proactive cards queue in the tick thread (`mpsc` from workers) and emit at a boundary — lull ≥8s or sentence-final char — with `MAX_HOLD_TICKS = 10` (30s) force-emit and drain-on-Stop. **Event payloads unchanged** — FE untouched. (Deliberately NOT shared with the L1 topic segmenter: live detector is coarse+cheap, batch segmenter is offline — coupling them would hurt both.)

## L5 — Agents & surfaces

- **Scheduled briefs** (`brief_runner.rs`, 60s tokio interval): `brief_schedules` uses **structured columns** (day_of_week/hour/minute — no `cron` crate, no new dep) + `brief_runs` (propose-accept staging; `meeting_ids` = ids only). Corpus = gated deterministic reads (`list_meetings_visible` window + commitments + facts); synthesis reuses `digest.rs::build_digest_prompt`; **quiet-if-empty**, max one run/schedule/day. Output: `EVENT_BRIEF_PROPOSED` → FE `BriefsStore` + `briefs/` feature; accept ⇒ vault export, dismiss ⇒ row deleted.
- **MCP client** (`connectors/mcp_client.rs` + `connectors/mcp.rs`): **hand-rolled JSON-RPC 2.0 (~150 lines) over existing reqwest/tokio — no new crates**; transports HTTP + stdio (SSE deferred). One `McpConnector` per configured server (`mcp_servers` table: per-server `consented` flag), riding the existing consent + redaction + egress-ledger seam with truthful per-server attribution. Discovered tools surface **only at Tier 3** as `mcp_<server>_query` with sanitized, 100-char-capped descriptions — **tool descriptions are untrusted input and are never interpolated into system prompts**; results truncated at `RESULT_BUDGET`. stdio servers = code execution: absolute-path-only + explicit trust warning in the add-server UI. **Lock-security review required (new egress class).**
- **Verify pass:** v1 stays **deterministic** (Anthropic's top verification tier): existing `extract_issue_keys` + `jira_lookup` + `judge`, extended with `judge_with_detail` and an idempotent `> [!verify]-` fenced callout (`apply_verify_callout`, byte-exact-undo, `enrich.rs` fence discipline) alongside the inline markers; session `verify_cache` in AppState (RAM-only, cleared on relock); persists via `upsert_note` (canonical, seals with the note) + vault re-export. LLM claim-extraction is explicitly **not** in v1.

---

## Phase / PR plan

| PR | Content | Gates |
|---|---|---|
| **1. P0 hotfixes** | P0.1 + P0.2 + P0.3 (redact heuristic, stub guard, caps/timeout/dedup/priority, `run_agentic_loop` opts param) | RED tests for all three; `cargo test --lib`; **lock-security** (redaction change); adversarial-verifier |
| **2. Eval gate bootstrap** | fixture + `RetrievalMode::Reranked` + `format_report_markdown` + `MURMUR_BAKEOFF_OUT`; user labels the 20 queries; **run baseline, commit artifact** | first committed numbers = the baseline every later PR beats |
| **3. L1 retrieval** | topic segmentation + augmentation + score fusion + temporal filter + reranker seam (prompted-Qwen) | re-run eval, artifact updated, recall@5 ≥ baseline; **lock-security** (new tables/purge paths) |
| **4. L2 memory** | relevance-filtered brief → consolidation job → memory import | RED tests; **lock-security** (`search_user_facts_visible`, import, rollup vault writes) |
| **5. L3 orchestration** | router + prompts.rs + JIT ask (flag) + compaction + history budget + JSON retry + native-JSON flag | eval re-run for JIT (answer faithfulness); **lock-security** (`get_meeting` tool) |
| **6. L4 live** | gatekeeper → bullets → boundary surfacing → bullets-as-Stage-1 | `cargo test --lib` + real-Mac recording session; **lock-security** (`live_bullets` seal semantics) |
| **7. L5 agents** | scheduled briefs → MCP client → verify callout | **lock-security** (MCP egress, verify cache); real-Mac for stdio MCP + Obsidian callout render |

Dependencies: PR1 first (touches `live.rs`/`agent.rs` seams others build on); PR2 before PR3 (no retrieval change without a baseline); PR3⊥PR4 can parallel; PR6 phases internally (gatekeeper → {bullets, boundary}); PR7's three items are mutually independent.

**Honest verification limits (say so in every DoD):** reranker latency, live boundary-timing UX, bullets quality, grammar-constraint feasibility on Qwen3-4B, MCP stdio lifecycle, Obsidian callout rendering — all need a real Mac (and some need real meetings); headless green is not proof for them.

---

## Consolidated open decisions (user input wanted)

1. **P0.1 heuristic aggressiveness** — conservative blocklist that may false-positive on two-word project names ("Atlas Project" survives only if blocklisted patterns don't match). Recommendation: ship conservative, expand blocklist from observed dev false-positives.
2. **P0.3 timeout: hardcode 30s or config key** (`brain_generation_timeout_secs`)? Recommendation: hardcode now, config later if older-Mac reports come in.
3. **`ask_jit_retrieval` default** — recommendation: false until the eval run compares JIT vs packed-corpus answer faithfulness on the real vault.
4. **Grammar constraint** — needs a real-Mac spike on Qwen3-4B + mistralrs 0.8.1 `Constraint::JsonSchema` with a tiny schema; stays flag-gated false until proven.
5. **Reranker k and timeout** — 10 candidates / 3s to start; first eval run decides (or drops k to 5). bge-reranker-via-candle stays deferred behind the trait.
6. **Eval fixture labeling** — needs YOU: ~20 real queries with expected meeting ids over your dev vault (protocol: `docs/RAG-BAKEOFF.md` Stage 1, then fill the fixture). This is the one step the agent can't do alone.
7. **Importance scoring cost** — LLM call per new fact at write time vs batch in the hourly job. Recommendation: batch in the job (zero pipeline latency), revisit after profiling.
8. **MCP SSE transport** — v1 = HTTP + stdio only; confirm no target server needs SSE before building it.
9. **`live_bullets_enabled` default** — recommendation: true when a local model is present (it's the substrate for better reactions), off on stub.
10. **Brief zero-egress strictness** — briefs use the current provider (consent-gated) by default; add `brief_zero_egress` flag only if you want briefs strictly local even with cloud configured.
11. **brain2 vs ClickUp Brain² naming** — unchanged hard blocker before any public copy.
