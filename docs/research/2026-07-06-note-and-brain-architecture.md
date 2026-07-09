# Murmur — Two-Stage Notes + Current-First Brain Cascade: Architecture Design

**Status:** Decision-ready architect design. Grounded against the current tree (`src-tauri/src`, branch `murmur`, shipped 0.7.5). Cite-by-symbol throughout; line anchors in `commands.rs`/`db.rs` drift (>8k lines) — grep the symbol.

---

## 1. Executive summary

Murmur today generates a note in a **single pass with a single egress**, and it deliberately mixes cross-meeting context *into that one generation prompt*. `pipeline.rs:summarize_and_export` unconditionally builds a gated cross-meeting grounding corpus (`orchestrate::orchestrate_context` → `related_context::build_related_context`) and hands `transcript + related_context + user_notes + vault_titles` to the model together via `template.rs:render_user_content`. Because the on-device model is weak, this pre-generation mixing is exactly the failure mode the small-model RAG literature warns about (context *utilization*, not retrieval, is the bottleneck), and it is the confirmed source of the "qwen-4B pasted another meeting's `## Action items`" bleed. The current defense — `grounding.rs:strip_ungrounded_action_items`, an always-on lexical word-overlap deletion of generated checklist units — is a hack that treats the symptom and risks deleting *real* content.

The **target replaces model-trust and lexical heuristics with structural separation**:

- **NOTE-GEN → two stages.** *Stage 1* generates the note from **only this meeting's transcript + the user's typed notes** — `related_context: None`, provably zero cross-meeting bleed (the `render_user_content` no-context path is already byte-identical for `None`). *Stage 2* is an **additive, idempotent, byte-exact-undo post-pass** over the finished note that appends cross-meeting `[[links]]` + a Related/Context section **without touching** the core action items/decisions — reusing the already-shipped `enrich.rs:apply_context_markers` writer and the `apply_note_enrichment` persist seam. `reference_gist` (task-free by construction) is repurposed as the Stage-2 link renderer, so a linked note *cannot* drag its action items in. Connector enrichment (`enrich.rs`) is folded in as **Lane B** of that same Stage 2, not duplicated.

- **BRAIN → current-first cascade.** *Tier 1* answers from the **current meeting in isolation** (live RAM buffer while recording; that meeting's segments/note when past). *Tier 2* broadens to the vault. *Tier 3* reaches connectors/web. The escalation is **structurally deterministic** (which tools each tier may reach, enforced by `GatedToolExecutor::specs()`), while retrieval *within* a tier stays model-driven — never prompt-nudge-only, which is today's failure mode. The answer surfaces **which tier answered**. The wrong-meeting bug ("o czym to spotkanie" → an arbitrary saved meeting) is killed by threading an explicit `meeting_id` FE→IPC→loop instead of relying on the recorder-lifecycle `state.current_meeting`.

- **CONTEXT MODEL.** The **durable/long-term substrate is solid and stays as-is** (SQLite canonical; facts/user_facts/chunks/entities/interactions uniformly derived, purged-on-seal, visibility-gated). The **working-memory + conversation layer is the thin spot** and gets a small, explicit model: a durable per-thread `meeting_id` binding, and a clear RAM-vs-DB split for the live buffer vs the finalized note.

**The single most important decision:** whether **Stage 2 is deterministic (link + `reference_gist`, zero egress)** — the strongest privacy posture and the recommended default — or uses an LLM prose pass behind the existing consent+redaction envelope. Everything else follows from that fork.

---

## 2. Current state (grounded)

### 2.1 Note generation — single-pass, single-egress, context-mixed

`pipeline.rs:run_inner` transcribes → merges → `insert_segments` (status `Transcribed`) → `build_transcript_feed` → delegates to `pipeline.rs:summarize_and_export` (`pipeline.rs:866`). That function, in order:

1. Resolves the fail-closed, redaction-wrapped Notes provider (`make_provider_resolved` → `RedactingProvider`, gated on `cloud_egress_consented`; cloud egress with no consent returns `Unavailable`).
2. `list_vault_titles`.
3. **Always** builds the cross-meeting corpus: `orchestrate::orchestrate_context(...)` (`pipeline.rs:930`) → `build_grounding_context` → `related_context::{salient_query, build_related_context}`. Double visibility-gated (`db.search_visible(...)` candidate gate + `db.get_note_if_visible(...)` body gate), self-excluding (`if m.id == this_meeting_id { continue }`), budgeted, weak-vs-strong split (`is_weak_provider` → link-only header; strong → `reference_gist` prose).
4. Reads `db.get_manual_notes(meeting_id)`.
5. Assembles **one** `SummarizeRequest{transcript, related_context, user_notes, vault_titles, template}`; `template.rs:render_user_content` (`template.rs:204`) emits the "Related prior notes" block, the user-notes skeleton, then the transcript — **all in one prompt**.
6. **The only egress in the whole pipeline:** `provider.summarize_with_meta(&request)`.
7. `finalize_note_markdown` (folds `## My notes` / enhance marker).
8. `grounding::strip_ungrounded_action_items(&markdown, &segments)` (`pipeline.rs:998`) — **always-on in auto-language**; deletes generated checklist/decision units at zero transcript overlap. `annotate_unverified` is opt-in (default OFF).
9. `upsert_note` → semantic index → derive title → auto-file subfolder → provenance/privacy frontmatter → atomic vault write → `seal_auto_filed_note` (if the target folder is locked) → build entities.

**Timeline and action-items are NOT in this pipeline** — timeline is `summarize::timeline::generate` (a separate command), action items are parsed on demand (`parse_action_items` via `get_action_items`/`patch_note_tasks`). A Stage-2 block that keeps checklist lines byte-identical won't disturb them.

**What's wrong:** cross-meeting context is injected *pre-generation into the weak model's one prompt* — the exact structural cause of the `## Action items` bleed. `strip_ungrounded_action_items` exists *only* to clean up that bleed and is a lexical hack that can delete real generated content.

### 2.2 Brain / @brain assistant — one vault-wide loop, wrong-meeting bug

`ask_assistant_text`/`ask_assistant_chat` → `run_assistant_query` (`transcribe/live.rs:424`) → `run_informational` (`transcribe/live.rs:513`) → `agent::run_agentic_loop`. Every turn advertises the **full** gated tool catalog (search_meetings, search_semantic, list_recent, get_meeting, commitments, dossier, + web/jira/slack/calendar). **There is no tier isolation** — the current meeting is only a system-prompt hint (a 6k `live_transcript` tail + typed notes); the model can freely call vault tools.

- Meeting scope = `state.current_meeting`, set **only** at `start_recording`, cleared at `stop_recording`. Idle ⇒ `None` ⇒ `meeting_id == ""`.
- The FE **never sends a meeting_id** for @brain (`ipc.service.ts:askAssistantChat` omits it; the store privately tracks `_meetingId`).
- **Wrong-meeting mechanism:** empty `meeting_id` ⇒ `gated_live_context` returns `("","")` ⇒ `assistant_system_prompt` falls into the "ground answers in the vault" branch ⇒ "what is this meeting about" is answered by `search_meetings`/`list_recent_meetings` describing an *arbitrary saved meeting*.
- Even while recording, current-meeting grounding is prompt-only and mitigation is model-obedience-dependent — the tool catalog is unchanged.
- Citations are `[[Title]]` scraped from concatenated tool output (`voice_action::extract_citations`); **no tier/source tag**.

Existing primitives worth reusing: `commands.rs:chat_meeting` (meeting-scoped, explicit `meeting_id`, direct transcript injection, one completion, no tools — a de-facto "Tier 1"), and `commands.rs:ask_vault` with its **agentic-attempt → deterministic floor** fallthrough (`ask_vault_agentic_attempt` → `ask_vault_floor`) — the escalation shape a tier ladder should mirror. `GatedToolExecutor::specs()` (`tools.rs`) **already filters the catalog per surface** (`has_app`, `note_drafts`, `allow_writes`) and `run()` re-enforces the allowlist — the exact seam for per-tier gating.

### 2.3 Context management — durable layer solid, working/conversation layer thin

- **Long-term (DB, durable):** SQLite canonical (`segments`, `notes`, `timelines`, `manual_notes`); everything derived (`assistant_interactions`, `facts`, `user_facts`, `note_chunks`, `entities`) is uniformly purged-on-seal + visibility-gated. Bitemporal facts/user-memory is a genuinely good long-term design (`user_memory::synthesize_brief`). **This is closed and well-factored — keep it.**
- **Session (RAM, `AppState`):** `unlocked_folders`, `master_kek`, `account_session` — clear lifetimes, solid.
- **Working (RAM, per-recording):** the **only** structured live-meeting memory is `state.live_transcript: Mutex<String>` (16k cap, 6k injected, mic-only, cleared at Stop). `state.current_meeting` is a *recording* pointer, not a *focus* pointer — the root scoping fragility.
- **Conversation:** the backend is **stateless per turn** — the FE resends the whole (12-turn-capped, `CHAT_CONTEXT_TURNS=12`) history each call; `run_assistant_query` re-snapshots config + unlocked-set + meeting-id fresh per turn; the agentic ReAct scratchpad is discarded at turn end. `assistant_interactions` exists for rehydration/attribution/purge, **not** server-side context reconstruction. The per-thread meeting binding lives *only* on the FE and is dropped at the IPC boundary.

### 2.4 The connector-enrichment feature (already built, headless)

`enrich_note_context_inner` (read/egress preview → `Vec<ContextHit>`) + `apply_note_enrichment_inner` (write). `enrich.rs:apply_context_markers` (`enrich.rs:55`) is a pure, deterministic, append-only, byte-preserving, **idempotent** (`apply(apply(x))==apply(x)`), **byte-exact-undo** (empty hits strips), injection-hardened (`sanitize()` neutralizes fence-forging) writer that fences one `> [!context]-` callout. Both commands are `meeting_is_unlocked`-gated before any egress or write; the write persists to **canonical `notes.markdown`** via `upsert_note` **and** re-exports via `export::overwrite_note`, so it **seals with the note** (deliberately not Re-Truth's vault-only path that drops on seal). **No FE surface yet** — IPC + models exist, no component calls them.

---

## 3. Target NOTE-GENERATION flow

Mapping the user's desired flow onto the two-stage model:

```
transcription
  → [Stage 1] isolated note  (THIS transcript + typed notes ONLY)   ← single gated egress, or fully local
  → [Stage 2] cross-meeting links/context (Lane A local + Lane B connectors)  ← additive, on finished note
  → final output (upsert_note canonical + atomic vault export + seal-if-locked)
```

"Enhance (typed notes)" is **not a separate stage** — it is part of Stage 1 (the manual-notes buffer already rides Stage 1 as the enhance skeleton or the append `## My notes`). Connector enrichment is **Lane B of Stage 2**, reusing `enrich.rs`.

### Stage 0 — Transcription (unchanged)
- **In:** audio. **Out:** merged segments persisted via `insert_segments`; `build_transcript_feed` → `TranscriptFeed{retrieval_text, summary_text, labeled}`; status `Transcribed`.
- **Egress:** none. **Seam:** `pipeline.rs:run_inner` (unchanged).

### Stage 1 — Isolated note (transcript + typed notes only)
- **In:** `feed.summary_text` + `db.get_manual_notes(meeting_id)` (as enhance skeleton when `notes_mode == "enhance"`, else folded as `## My notes` after generation) + `vault_titles`. **No `related_context`.**
- **Out:** a note grounded **provably** only in its own transcript; `upsert_note` (status `Summarized`).
- **Egress:** the **single** gated egress `provider.summarize_with_meta` through the `make_provider_resolved` fail-closed `cloud_egress_consented` gate + `RedactingProvider`. For a fully-local provider, nothing leaves.
- **Seam to build:** in `pipeline.rs:summarize_and_export`, **skip the `orchestrate_context` call (`pipeline.rs:930`)** and set `SummarizeRequest.related_context = None`. `render_user_content` already emits a **byte-identical no-context prompt** for `None` (test `render_user_content_none_is_unchanged`, `template.rs:305`) — so Stage 1's prompt carries only transcript + user_notes + vault_titles. Everything from `provider.summarize_with_meta` through `upsert_note` stays as Stage 1.

**RIP OUT (for Stage 1):**
1. The always-on injection: the `orchestrate_context` call (`pipeline.rs:930`), the `related_context` wiring into `SummarizeRequest`, and the "Related prior notes" block in `render_user_content` (`template.rs:204`, the `if let Some(ctx)` arm).
2. `grounding::strip_ungrounded_action_items` + `strip_applies_for_language` + its call at `pipeline.rs:998`. Its **sole purpose** is cleaning cross-meeting bleed from the injection being removed; once Stage 1 is transcript-only, the bleed class is gone by construction and the lexical deletion (which risks removing real generated content) is dead weight. **This is the "DROP Layer-2 hack" decision, realized.**

**KEEP (orthogonal):** `annotate_unverified` (opt-in `ground_summary`, grounds against *this* transcript only) and the `[UNCLEAR]` preventive marking in `build_transcript_feed`. With cross-meeting injection gone, Stage-1 transcript-only grounding becomes the *primary* anti-hallucination guard — a candidate to promote `annotate_unverified` to default-ON (open question).

### Stage 2 — Cross-meeting linking + context (two lanes, one writer)
A **post-pass over the finished Stage-1 note**, modeled on `apply_note_enrichment_inner`. Two lanes fan out to sub-sources but share **one writer + one persist seam + one strip**.

**Lane A — local cross-meeting linking (zero egress).**
- **In:** the finished note markdown + a gated retrieval over owned notes (repurpose `related_context::salient_query` + `build_related_context`, double-gated on the live unlock set, self-excluding; optionally the `orchestrate_context` reasoner plan).
- **Out:** an additive fenced section (e.g. `> [!related]-` / `## Related notes`) of `[[Title]]` + **`reference_gist`** one-liners. `reference_gist` (`related_context.rs:200`) is **task-free by construction** (never emits an action-items/decisions/tasks section, EN+PL) — so a linked note *cannot* drag its checklist in. Model cross-meeting links as `ContextHit{source:"Murmur", detail:gist, url:"[[Note]]"/obsidian://}` — **no new DTO**; the writer already renders `(via <source>)` + link.
- **Egress:** **none** (deterministic link + gist). Because it never egresses, Lane A is **auto-eligible on finalize**.

**Lane B — connector enrichment (egress, on-demand).**
- **In:** the finished note; the existing `enrich_note_context_inner` path (Jira issue-key lookup + free-text title search of Slack/web through the redaction+ledger `ConnectorRegistry`).
- **Out:** the `> [!context]-` callout of `ContextHit`s.
- **Egress:** through the registry's fail-closed consent + `redact_connector_query` + content-free ledger row. **Stays explicit/on-demand** — a one-time auto-consent would weaken "nothing leaves unless you ask".

**Shared writer/persist (build once, reuse for both lanes + fold in `verify.rs`):**
- **Writer:** `enrich::apply_context_markers` — append-only, idempotent, byte-exact-undo, sanitize-hardened, seal-safe. Emit **distinct managed blocks** per lane (`murmur:links` fence for Lane A, existing `murmur:context` fence for Lane B). Consolidate the older `verify.rs:apply_verify_markers` onto this engine — do **not** ship a third fence format.
- **Persist seam:** the tail of `apply_note_enrichment_inner`: `meeting_is_unlocked` gate → `upsert_note` (DB canonical, seals) → `export::overwrite_note` (vault re-export if `exported_path`) → `NoteDto`. **Cross-meeting links MUST use this DB-canonical path** — not Re-Truth's vault-only `overwrite_note` (which drops on seal). This is the #1 integration trap.
- **One strip-all step:** at the start of Stage 2, strip *all* managed blocks (links + context + verify) so re-running is idempotent across lanes (today each feature strips only its own fence).

**Where Stage 2 runs in the AUTO pipeline (open question, recommend deferred pass):** run Lane A as a **deferred post-`Exported` pass** (like enrich) rather than inline before `export::write_note`. This sidesteps the `SealInto` auto-file branch (`seal_auto_filed_note`) and the no-vault branch, and matches the "self-healing link graph" semantics — re-running Lane A over old notes re-links them against current DB state without re-summarizing (mirror `resummarize_existing`, link-only). The tradeoff: the *first* exported `.md` won't carry links until the deferred pass runs (seconds later).

**Egress/gate invariants preserved across both stages:** any content leaving the device passes `make_provider_resolved`'s fail-closed consent gate + `RedactingProvider`; any cross-meeting read routes through `search_visible`/`get_note_if_visible`/`meeting_is_visible` on the live unlocked set. A deterministic Stage 2 (Lane A) **egresses nothing**; an LLM Stage 2 re-enters the same gated single-egress envelope.

---

## 4. Target BRAIN / CONVERSATION cascade

```
conversation turn (with explicit meeting_id)
  → Tier 1: current meeting IN ISOLATION   (live buffer if recording, else this meeting's note+segments)
  → Tier 2: broaden to vault notes          (existing vault-wide loop/tools)
  → Tier 3: connectors / web                (consent+egress-gated tools)
  → answer + "answered_from: TierN" badge + tier-appropriate citations
```

**Never amputate cross-note search** — Tier 2/3 keep the full existing capability; the cascade just gives it the right *order* and *default*.

### Escalation mechanism — recommend **structurally-deterministic escalation, model-driven retrieval**

The genuine fork is *how* a tier decides "I can't answer, escalate". Two options:

- **(A) Deterministic tool-gating (RECOMMENDED).** Each tier runs `run_agentic_loop` with a **scoped executor** that advertises only that tier's tools (`GatedToolExecutor::specs()` + a new `scope`/`tier` field). Within the tier, tool choice stays model-driven. The tier's system prompt says "answer ONLY from this tier; if not answerable reply exactly `{\"answer\":\"__ESCALATE__\"}`". The caller detects the sentinel and re-runs at the next tier — reusing the proven `ask_vault_agentic_attempt` fallthrough shape, chaining tiers instead of a single loop→floor.
- **(B) Pure model-driven** (one loop, all tools, prompt says "prefer current meeting"). **Rejected** — this is *exactly today's failure mode*: prompt nudges do not keep a weak model in scope, and it produces the wrong-meeting bug.

**Justification:** the small-model literature is decisive that weak models are easily distracted and cannot be trusted to self-restrict scope via prompt text. The escalation *boundary* (which tools are reachable) must be **code-enforced** (`run()` already hard-rejects un-advertised tools). The *retrieval within a tier* can stay model-driven (honoring "no hardcoded routing"). This is "deterministic escalation, model-driven retrieval".

**Escalation signal:** the caller inspects `outcome.answer` for the `__ESCALATE__` sentinel — `Ok(Some(sentinel))` = "no answer here, go up"; `Ok(None)` still = non-convergence (out of steps) → floor within the tier. Optionally add a typed `no_answer` variant to `AgentOutcome` for cleanliness. This distinguishes "this tier has no answer" from "the loop ran out of steps", which today are conflated (`agent.rs` returns `Ok(None)` for both).

### How `meeting_id` threads through (kills the wrong-meeting bug)
- **Seam 1:** add `meetingId?: string` to `ipc.service.ts:askAssistantText/askAssistantChat`; pass `this._meetingId()` from `meeting-conversation.store.ts`. Add `meeting_id: Option<String>` to `commands.rs:ask_assistant_text/ask_assistant_chat`; thread through `run_assistant_query` → `run_informational`, **resolving as `fe_meeting_id.or(state.current_meeting)`** so a live recording still works and a past/anchored thread scopes correctly. `GatedToolExecutor.meeting_id` and `gated_live_context` already key on this id.
- **Durable binding:** persist the thread's `meeting_id` on the `assistant_interactions` row (`persist_interaction` currently keys on the *current recording* meeting only) so a thread always answers about its own meeting even when idle. **Resolution precedence** (open question): a bound past-meeting thread should win over `state.current_meeting` when a *different* meeting is recording.

### Tier grounding sources
- **Tier 1, LIVE meeting:** the `live_transcript` RAM tail — segments aren't persisted until Stop, so a DB-reading tool returns nothing during recording. Tier 1 must read the RAM buffer (single-shot completion over the buffer, like `chat_meeting`, is likely cleaner than a loop here).
- **Tier 1, PAST meeting:** that meeting's note + segments — exactly `chat_meeting`'s read shape (`get_segments` + `get_note_if_visible`, gated by `meeting_is_unlocked`).
- **Tier 2:** the existing vault-wide loop/tools (search_meetings/semantic/get_meeting/list_recent/commitments/dossier).
- **Tier 3:** connectors/web (already consent+egress-gated via `has_app` in `specs()`).

### Sources reflect the tier
Add `answered_from`/`tier` to `AgentOutcome` + `VoiceActionResult` + `AskVaultResult`, set **deterministically from which tier converged** (the ladder knows) — never string-sniffed. Tier 1 must **add the current meeting's own `[[Title]]`** to citations (prompt-injected content produces no wikilink in `gathered`; resolve via `db.get_meeting(meeting_id).title`). Strengthen Tier 3 by capturing connector loud-lines the way `voice_action::rag_answer` does, so web/jira/slack answers are attributed even when the model omits "(via …)". Surface the badge to the user (silent auto-escalation vs a visible "not in this meeting, searching your vault…" chip is an open UX question; the tool-trace event stream can carry a tier badge).

### Reuse of the note pipeline's retrieval
Tier 2 retrieval and Stage-2 Lane-A retrieval are the **same substrate** — `search_visible`/`search_hybrid_visible` + `salient_query` + (optionally) the `orchestrate_context` reasoner plan, all visibility-gated. Build the gated-retrieval helper once; both the note Stage-2 and brain Tier-2 call it. `orchestrate.rs` is the one existing staged-retrieval-with-planner and the natural place to generalize.

### Preserve the correct vault-wide Ask page
`commands.rs:ask_vault` (vault-scoped executor `meeting_id:""`, `ASK_MAX_STEPS=6`, agentic-attempt→floor) is **already correct** for a *deliberately vault-wide* surface — it is not the wrong-meeting bug. **Leave it as-is.** The cascade applies to the *in-meeting @brain surface* where "current meeting" is the intended default. `chat_meeting` and `run_recipe` are already meeting-scoped single-shots; the cascade can subsume the in-meeting @brain path while leaving these standalone commands intact (open question whether to unify them).

---

## 5. Unified CONTEXT-MANAGEMENT model

**Direct answer to the user's question:** the long-term/durable substrate is **already solid — do not rebuild it**. The **working-memory + conversation layer is thin and fragmented** and is the one place the target work needs a new (small, layered) model. It is *not* a rewrite.

### What is held where today

| Horizon | Where | Contents | Lifetime | Verdict |
|---|---|---|---|---|
| Long-term | SQLite (SQLCipher) | `segments`, `notes`, `timelines`, `manual_notes` canonical; `facts`/`user_facts`/`note_chunks`/`entities`/`assistant_interactions` derived | durable; derived layers purged-on-seal, visibility-gated | **Solid, closed** |
| Session | `AppState` RAM | `unlocked_folders`, `master_kek`, `account_session` | session / until relock / until logout | **Solid** |
| Working (live meeting) | `AppState` RAM | `live_transcript` (single flat string, 16k, mic-only); `current_meeting` (recording pointer) | cleared at Stop | **Thin** |
| Conversation | **FE only** | 12-turn window resent per call; agentic scratchpad discarded per turn | ephemeral; `assistant_interactions` for rehydration only | **Fragmented** |

The gap sits **exactly on the boundary the target work occupies**: "holding the evolving state of a live meeting (two-stage note)" and "an ongoing brain conversation (cascade across turns)".

### Target working-memory model (layered over the solid store)

Map onto the 2025 RAM/disk memory analogy (Anthropic context-engineering + Weaviate/MemGPT): **context window = RAM (working memory); SQLite = disk (long-term)**. Concretely:

1. **Decouple "current focus" from "recording".** Introduce a focus pointer (viewed/anchored meeting) distinct from `state.current_meeting` ("Some while recording"). The brain's Tier-1 "this meeting" resolves against **focus**, with a clear precedence rule (bound thread meeting > focus > recording). This is what makes the cascade deterministic when idle.

2. **Per-thread meeting binding is durable.** Persist `meeting_id` on the `assistant_interactions` thread row (§4). The conversation object stays **FE-owned by default** (the "FE owns, backend stateless" rule is load-bearing simplicity) — but the backend gains a **durable thread→meeting binding** and can reconstruct a rolling summarized history + accumulated citations from the already-gated `assistant_interactions` on cold start. Do **not** move the whole conversation into the backend unless a measured latency/quality need appears.

3. **Two-stage note working memory = RAM buffer + finalized DB note.** Stage 1's working set is *only* transcript+template; Stage 2's working set is *only* draft+filtered-corpus — **neither stage ever holds full transcript AND cross-doc bodies at once** (sub-agent isolation / clean context windows). Whether Stage 1 needs a *durable* staging column (survives crash) or a RAM buffer + spill-file (mirroring `spill_writer`) is an open question — but any durable Stage-1 plaintext MUST be seal-gated like segments/notes.

4. **Compaction tuned to the weak model's small window.** The agentic loop already implements Anthropic's compaction discipline (`RESULT_BUDGET=4000`/step, no-repeat guard, treat tool results as data). Add an explicit "summarize when the transcript nears the window" trigger for the weak model, and keep the stage-2 refiner (if any) off the caption tick (skip-if-busy worker, like reactions).

5. **Mandatory invariant inheritance.** Every new stage/buffer/tier MUST: route reads through `meeting_is_visible`/`visibility_clause`/`meeting_is_unlocked`; clear/gate on relock (as `gated_live_context` + `clear_live_transcript_if_idle` already do); be purged-on-seal if derived; egress only through the redaction+consent firewall; log no PII.

**Verdict:** add a small **`WorkingSet`/focus-pointer + durable thread binding** layer; preserve everything durable. Not a new unified store — a thin working-memory model over the canonical one.

---

## 6. Integration with the concurrent connector-enrichment work

**`enrich.rs` is the EXTERNAL half of Stage 2 (Lane B) — adjacent, not identical, to cross-meeting linking (Lane A).** The target is **one Stage-2 orchestrator, two lanes, one writer, one persist seam, one strip** — do **not** build two parallel write pipelines.

- **Do not duplicate the writer.** Lane A (local links) routes through `enrich::apply_context_markers` (or a lightly-generalized sibling emitting a `murmur:links` fence) — inheriting idempotency, byte-exact undo, sanitize-hardening, seal-safety for free. Fold `verify.rs:apply_verify_markers` into the same engine (three fence formats today → one).
- **Do not duplicate the persist seam.** Lane A links persist via the `apply_note_enrichment_inner` tail (`upsert_note` DB-canonical → `overwrite_note`), **not** Re-Truth's vault-only path — or links drop on seal (the flagged trap).
- **Trigger asymmetry (correct by privacy):** Lane A (zero egress) is **auto-eligible on finalize**; Lane B (egress) **stays on-demand**. The research doc's rejection of AUTO applies only to the egress path.
- **FE:** enrich has **no UI yet**. Build **one** Stage-2 review panel (clone `verify-panel.component.ts`) showing Lane A local link candidates (free) and Lane B connector hits (the egress button) as one consolidated block.
- **Coordination:** if the enrich FE ships before this redesign, build it as the Lane-B panel from the start so it doesn't have to be reworked. The `ContextHit{source, detail, url}` DTO already covers Lane A (`source:"Murmur"`) — no schema change needed. Confirm with whoever owns the enrich branch that the `murmur:links` vs `murmur:context` fence split and the single strip-all step land together.

---

## 7. What competitors / patterns validate or warn against

1. **Single-source draft is the industry norm — validate Stage 1.** Every meeting tool (Granola, Fireflies, Otter, Fathom, tl;dv) generates the note from *that meeting's transcript alone* and keeps cross-meeting context **out of the note body**. Granola's Enhance explicitly uses "that meeting's transcript only". Murmur's current in-prompt cross-note mixing is the *unusual* choice — the two-stage split moves Murmur *toward* the validated pattern. **Differentiation:** Murmur's Stage-1 draft can be **fully local/on-device** (fast, private) where competitors are cloud.

2. **Prompt chaining beats a single mixed prompt (academic).** arXiv 2406.00507 — a unified "stepwise" prompt only *simulates* refinement; genuine chaining (draft → refine) wins. Self-Refine reports ~20% gains from a real second pass. This is the direct justification for splitting Stage 1 / Stage 2.

3. **For weak models the bottleneck is context *utilization*, not retrieval — warn against pre-generation mixing.** arXiv 2603.11513 + Lost-in-the-Middle (2307.03172) + The Distracting Effect (ACL 2025): small models are far more easily distracted by adjacent passages; the fix is **fewer, higher-precision passages + structural separation** of retrieved content from the generation task. This is precisely why cross-doc context must leave the Stage-1 draft prompt — and why the fix is *structural*, not the lexical `strip_ungrounded_action_items`.

4. **Two-scope Q&A + tier-tagged citations are table-stakes.** Fireflies ("Global AskFred" vs per-meeting), Fathom (account-wide + link-to-exact-moment), tl;dv (single vs multi-meeting), Otter (cross-conversation Chat) all split "this meeting" vs "workspace" and cite back to source. Murmur's cascade with an `answered_from` badge is the **auto-scope superset** of the industry two-scope split. **Differentiation:** every retrieval seam is **visibility-gated (crypto-sealed)** — no competitor gates retrieval by a per-folder lock.

5. **Nobody auto-injects live external context into the note body — validate on-demand Lane B.** Notion/ClickUp/Glean fold connectors into the *answer*; Granola/Fireflies push *out* to tickets; Fellow's `/jira` is user-confirmed. Auto-appending live context invites the #1 complaint (verbosity/staleness). Murmur's dated, foldable, on-demand `> [!context]-` callout is the right posture. **Differentiation:** owned Obsidian `.md` with `[[wikilinks]]`/block-refs, no cloud index, no multi-hour indexing lag (Notion ~3h), no manual reindex (Copilot).

6. **Activate the dormant semantic recall.** Every knowledge competitor ships embeddings; Murmur's e5 is present-but-dormant and the repo bake-off showed recall@5 1.00 semantic vs 0.42 FTS. Stage-2 Lane A "related meetings" and Tier-2 vault recall are the place to turn it on — **on-device**, matching Obsidian Smart Connections/Reflect, which is Murmur's uncopyable lane (local-first + on-device embeddings + far-side capture + sealed retrieval).

**Anti-patterns to avoid:** Otter-style verbosity (default concise + foldable), cloud-indexing all content, indexing lag, manual reindex.

---

## 8. Phased implementation plan

Each phase: goal, files/symbols, verification, gates. Backend (Rust) and FE (Angular) are largely disjoint → parallelize, serialize shared files. Lock/egress-touching phases require the **lock-security-reviewer** as a second gate; every phase requires the **adversarial-verifier** (implementer never self-certifies). Real gates: `cargo test --lib`, `npx ng lint`, `npx ng build`, `scripts/ci.sh`. RED-before-GREEN for every bug fix.

### Phase 0 — Land the separate lock/data-loss fix (S, no dependency)
- **Goal:** ship the `move_into_locked_folder` session-restore fix (the already-built lock fix). It is **independent** of the note/brain redesign and should not be entangled with it.
- **Files:** the `move_into_locked_folder`/session-restore path (per the established decision this is separate).
- **Verify + gate:** `cargo test --lib`; **lock-security-reviewer required** (lock-touching); adversarial-verifier RED-before-GREEN for the data-loss regression.

### Phase 1 — Stage 1 isolation + rip out the Layer-2 hack (M, backend)
- **Goal:** Stage-1 note is provably transcript-only; delete the lexical hack.
- **Symbols:** `pipeline.rs:summarize_and_export` — remove the `orchestrate_context` call (`pipeline.rs:930`), set `SummarizeRequest.related_context = None`; remove the "Related prior notes" arm in `template.rs:render_user_content`; delete `grounding::strip_ungrounded_action_items` + `strip_applies_for_language` + the call at `pipeline.rs:998`.
- **Verify:** existing `render_user_content_none_is_unchanged` proves byte-identical prompt; add a RED-before-GREEN test that a note generated with a "poisoned" vault (another meeting with `## Action items`) contains **zero** of that meeting's action items (the bleed regression, now structurally impossible). Dev-app boot clean.
- **Gate:** adversarial-verifier (hunt content-loss + bleed); **lock-security-reviewer** (egress surface changed — one fewer thing in the prompt, confirm no gate weakened).
- **Note:** this phase alone eliminates the bleed class and retires `strip_ungrounded_action_items`. Consider promoting `annotate_unverified` to default-ON here (separate decision).

### Phase 2 — Stage 2 shared writer + persist seam + Lane A local linking (M–L, backend)
- **Goal:** additive cross-meeting `[[links]]` + Related section on the finished note, deterministic, zero-egress; unify the writer.
- **Symbols:** generalize `enrich::apply_context_markers` to emit a `murmur:links` fence (Lane A) alongside `murmur:context` (Lane B); add a strip-all-managed-blocks step; new `link_related_notes` pass mirroring `apply_note_enrichment_inner` (gate → retrieve via `related_context::salient_query` + `build_related_context` → render `[[Title]]` + `reference_gist` as `ContextHit{source:"Murmur"}` → `upsert_note` → `overwrite_note`). Run as a **deferred post-`Exported` pass**. Fold `verify.rs:apply_verify_markers` into the shared engine.
- **Verify:** idempotency test `apply(apply(x))==apply(x)` across all three fences; `enriched_note_seals_and_restores_byte_identical`-style seal round-trip for the links block; a test proving Lane A links contain **no action-items** (guaranteed by `reference_gist`); self-exclusion + visibility-gate tests (reuse `build_related_context_excludes_sealed_until_unlocked`).
- **Gate:** **lock-security-reviewer required** (persist path + seal-safety + visibility-gated retrieval); adversarial-verifier (idempotency, byte-exact undo, no plaintext left in a sealed dir).

### Phase 3 — Stage-2 Lane B integration + unified review panel (M, backend light + FE)
- **Goal:** fold the existing connector enrichment in as Lane B; build the one Stage-2 review panel.
- **Symbols:** `enrich_note_context_inner`/`apply_note_enrichment_inner` (already built) become Lane B under the shared strip/writer/persist; FE clones `verify-panel.component.ts` into a Stage-2 panel showing Lane A (free) + Lane B (egress button) in one consolidated block; wire `ipc.service.ts:enrichNoteContext/applyNoteEnrichment` (currently unreachable).
- **Verify:** `enrich_commands_refuse_a_sealed_meeting` still passes; Playwright against `:1420` with mocked `invoke` drives the panel; `ng lint` + `ng build` (16k budget).
- **Gate:** **lock-security-reviewer** (egress lane); adversarial-verifier (fence-forging via hostile Slack/web hits — `sanitize` coverage).

### Phase 4 — Brain: thread meeting_id + kill wrong-meeting bug (S–M, backend + FE)
- **Goal:** explicit, durable meeting scope for @brain.
- **Symbols:** Seam 1 — `ipc.service.ts:askAssistantText/askAssistantChat` add `meetingId`; `meeting-conversation.store.ts` passes `_meetingId()`; `commands.rs:ask_assistant_text/ask_assistant_chat` add `meeting_id: Option<String>` → `run_assistant_query`/`run_informational`, resolve `fe_id.or(current_meeting)`; persist `meeting_id` on the `assistant_interactions` thread row (`persist_interaction`).
- **Verify:** RED-before-GREEN — "o czym to spotkanie" on an idle past-meeting thread returns *that* meeting, not an arbitrary one; a live-recording thread still scopes to the recording; a bound past thread wins over a *different* recording meeting (precedence rule).
- **Gate:** adversarial-verifier (the specific wrong-meeting reproduction); lock-security-reviewer (Tier-1 reads gated).

### Phase 5 — Brain: tier ladder (deterministic escalation) (L, backend)
- **Goal:** Tier 1 (isolated) → Tier 2 (vault) → Tier 3 (connectors), structural tool-gating.
- **Symbols:** add a `scope`/`tier` enum field to `GatedToolExecutor` + a `specs()` filter arm; restructure `run_informational` into a Tier1→Tier2→Tier3 ladder with the `__ESCALATE__` sentinel (mirror `ask_vault_agentic_attempt`→floor, chained); Tier-1 LIVE = single-shot over `live_transcript` (like `chat_meeting`), Tier-1 PAST = `chat_meeting` read shape; per-tier step budgets (T1 1–2, T2 3–4, T3 2–3) to stay live-safe; add `answered_from`/`tier` to `AgentOutcome`/`VoiceActionResult`/`AskVaultResult`; Tier-1 injects the current meeting's own `[[Title]]`; strengthen Tier-3 connector loud-line attribution (like `rag_answer`).
- **Verify:** each tier's executor advertises only its tools (`run()` rejects higher-tier tools); escalation fires only on the sentinel, not on generic non-convergence; tier badge is set deterministically; Playwright drives a question answerable only at Tier 2 and confirms the badge + escalation trace.
- **Gate:** **lock-security-reviewer required** (every tier read gated; Tier-3 egress consent-gated); adversarial-verifier (a Tier-1 question must NOT reach vault tools — prove the model *cannot*, not just *shouldn't*).
- **Preserve:** `ask_vault` unchanged (deliberately vault-wide).

### Phase 6 — Context-management layer: focus pointer + working-memory discipline (M, backend)
- **Goal:** decouple focus from recording; formalize the working-memory model.
- **Symbols:** add a focus pointer to `AppState` distinct from `current_meeting`; precedence rule (bound thread > focus > recording); optional compaction trigger for the weak model; confirm relock clears every new buffer (extend `clear_live_transcript_if_idle` discipline).
- **Verify:** focus-vs-recording precedence tests; relock clears the new working set; no PII in logs.
- **Gate:** lock-security-reviewer (clear-on-relock); adversarial-verifier.

### Phase 7 (optional) — Activate dormant e5 semantic recall for Lane A + Tier 2 (M–L, needs model)
- **Goal:** semantic "related meetings" + vault recall on-device.
- **Symbols:** `embed.rs:rrf_fuse` / `db.search_hybrid_visible` behind the same gated-retrieval helper; rerank-and-reduce with a tight passage budget for the weak model.
- **Verify:** real-vault (incl. Polish) recall eval, not just the small test corpus; **needs the model present + a real Mac** — cannot be fully verified headless (honesty bar).
- **Gate:** adversarial-verifier; lock-security-reviewer (retrieval gated).

**Dependencies:** Phase 0 independent (ship first). Phase 1 → Phase 2 → Phase 3 (Stage-2 chain). Phase 4 → Phase 5 → Phase 6 (brain chain). Phase 1 and Phase 4 can run in parallel (disjoint files). Phase 7 depends on Phase 2/5 and the embed model.

**How the reverted Layer 2 folds in:** the `strip_ungrounded_action_items` removal is **Phase 1** — it is retired the moment Stage 1 is transcript-only, because its only job was cleaning the injection being removed. **How the built lock fix folds in:** it is **Phase 0**, shipped independently, entangled with nothing here.

---

## 9. Risks + open questions

**Risks**
- **Strong-provider regression:** moving `related_context` out of the draft may *reduce* quality for strong cloud providers (which benefit from cross-doc grounding and aren't distracted). May need a **provider-tiered pipeline** (strong = single-pass with context; weak = strict two-stage) rather than a uniform split. Unverified — needs the notes bake-off on a real Mac.
- **Latency/thermal:** a genuine second pass (if Stage 2 uses an LLM) doubles inference on Apple Silicon; deterministic Lane A avoids this. No measured on-device numbers for qwen3-4b draft-vs-enrich delta.
- **Weak-model escalation gate:** Self-RAG/FLARE assume a model calibrated enough to say "I don't know / escalate". The `__ESCALATE__` sentinel on qwen3-4b is unproven; may need a deterministic coverage backstop. Structural tool-gating de-risks this (the model can't reach the wrong tier even if it mis-judges).
- **Seal-safety of new writes:** any Stage-2 or Tier read that writes/reads plaintext in a sealed dir is a leak/loss bug. The `upsert_note`→`overwrite_note` (not vault-only) discipline and the visibility gates are mandatory — hence lock-security-reviewer on Phases 2/3/5/6.
- **Line-anchor drift:** `commands.rs`/`db.rs` >8k lines; every citation here is by *symbol* — grep, don't trust numbers.

**Open questions** (technical, resolvable in-build)
- Durable Stage-1 staging column vs RAM buffer + spill-file? (Any durable plaintext must be seal-gated.)
- Deferred post-`Exported` Lane A pass vs inline before `export::write_note`? (Deferred recommended for the `SealInto`/no-vault branches.)
- Should Lane A backfill/refresh over historical notes, and what triggers it (new meeting, manual, cron)?
- Losing inline body `[[Title]]` citations (Stage-2 links in a separate section) — acceptable, or should Stage 2 also inject wikilinks into the body?
- Tier-1 LIVE single-shot vs small loop with a `get_current_meeting` tool (DB tools return nothing during recording).
- Precedence when a past-meeting thread is open while a *different* meeting records.
- Promote `annotate_unverified` to default-ON in Phase 1 (now the primary anti-hallucination guard)?
- Is GBNF/JSON constrained decoding actually wired through `LocalReasoner::structured`, or schema-in-prompt + `parse_first_json`? (Apply to extract legs only — "alignment tax" on free prose.)

**Cannot be verified headless (honesty bar):** on-device draft-vs-enrich quality/latency, real-vault (Polish) semantic recall, Touch ID / lock-at-rest / screen-share auto-relock — need a **signed build on a real Mac** with the model present.

---

## 10. Decisions needed from the user

The genuine forks (things the user must choose, not things the architect can decide):

1. **Stage-2 mode (THE central fork):** deterministic (link + `reference_gist`, **zero egress** — recommended) vs an LLM prose pass behind the consent+redaction envelope. Everything downstream depends on this.
2. **Provider-tiered vs uniform note pipeline:** strict two-stage for everyone, or two-stage only for the weak on-device model while strong cloud providers keep a single-pass with cross-doc context.
3. **Lane A auto vs on-demand:** auto-run local cross-meeting linking on finalize (zero egress, so the privacy objection doesn't apply — recommended) vs keep it user-invoked like Lane B.
4. **Escalation surfacing (UX):** silent auto-escalation across tiers vs a visible "not in this meeting → searching your vault…" chip.
5. **Conversation ownership:** keep FE-owned + backend durable thread→meeting binding (recommended, minimal) vs move the conversation working set into the backend as source of truth.
6. **Cascade scope:** does the current-first cascade subsume `chat_meeting`/`run_recipe`, or stay a record-screen-only @brain feature with those standalone commands intact?
7. **Activate dormant e5 semantic recall now (Phase 7) or defer** until the two-stage/cascade structure lands.
