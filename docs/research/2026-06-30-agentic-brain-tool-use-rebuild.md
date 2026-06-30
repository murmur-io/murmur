<!-- Generated 2026-06-30 via /research (murmur-researcher fan-out, 5 angles). Pricing/funding/version = point-in-time. -->
# Research: Rebuild the in-meeting assistant's decision engine from a hardcoded intent router to a model-driven agentic tool-use loop (voice + text, cloud + local)

## TL;DR / Verdict

**Yes — rip out the hardcoded router, and make the MODEL decide, but do it as an ADDITIVE bounded tool-use loop built on the brain seam's existing `structured()` primitive — NOT as native vendor tool-calling, and NOT via the `claude` CLI + our MCP server.** Five independent angles converged on one architecture:

1. **One new seam.** Add a default trait method `LocalReasoner::agentic(system, user, executor, max_steps) -> AgentOutcome` that drives a small, bounded loop over the *already-shared* `structured()` method. `CloudReasoner` / `MistralReasoner` / `StubReasoner` get it **for free and identically**. The loop lives in *our* Rust, over one gated `ToolExecutor`.
2. **The loop preserves every privacy invariant by construction**, because each turn still routes through `make_provider` → consent gate + `RedactingProvider`. The model emits only `{tool, args}` strings; it can never touch the host-held `unlocked` set, so a sealed meeting stays invisible. This is the decisive reason to build the loop on `structured()` rather than on a vendor-native path.
3. **VETO the "`claude` CLI pointed at our local MCP server" route** (two independent agents, emphatic). It uploads raw, gated-but-**un-redacted** meeting content to Anthropic outside the firewall, and forces unsealing the hermetic `--disallowedTools` posture. Categorically off the table for any Murmur-initiated cloud loop.
4. **Local stays bounded.** mistral.rs *does* have a full tool-calling stack, but reliability at Q4 + the Bielik template are unproven and a multi-round loop on a 14B is 10–25 s — too slow to feel live. Voice = **1 round** (decide → parallel-execute → synthesize); the note path can afford 2–3. The existing `orchestrate.rs` single-round plan is literally the `max_steps=1` case and stays as the **deterministic FLOOR**.
5. **The hardcode is demoted, not deleted.** `parse_voice_intent` / `handle_voice_action` / `rag_answer` become the stub/no-consent fallback (the `orchestrate.rs:123` pattern). Net hard deletions ≈ none → the migration is flag-gated, dual-path, and rollback is a flag flip. `cargo test --lib` stays green throughout.
6. **Text input is trivial once the brain exists.** Both voice (transcribed → `String`) and a new text composer (typed `String`) call the same `run_assistant_turn(command)` entry. One brain, two inputs.
7. **Streaming UX is a Tauri `Channel`, not events**, carrying phased deltas (Started/Thinking/Tool{running,done,count}/Token/Done). The one hard constraint: cloud token streaming needs a redaction **hold-back buffer** so a `⟪NAME_n⟫` placeholder is never split mid-token.

**Confidence:** high on the seam shape and the security verdict (both are direct extrapolations of already-shipped patterns + code-grounded); **medium** on local multi-step reliability and live latency (real-Mac-only — the honest bar).

---

## What we already have (from repo, with file:line)

**The hardcode to kill — exactly as the user described:**
- `audio/wake.rs:273` `parse_voice_intent` — deterministic PL+EN keyword/prefix matcher → fixed `VoiceIntent` enum {Research, SlackSearch, Recall, CreateReminder, NoteAside, Unknown} (`wake.rs:255-268`). The model never decides which bucket.
- `voice_action.rs:108` `handle_voice_action` — a `match` over that intent. Research/Recall → `rag_answer` (`voice_action.rs:257-509`) runs a **code-chosen, serial fan-out**: every query gets `SearchMeetings` + `SearchSemantic` (+ `GetEntityDossier` for recall) in a `for` loop (`:288-302`), plus intent-gated web (`:329`) and calendar (`:371`) legs, then **one** `reason()` synthesis call (`:475`). The legs are picked by code, not the model.
- `voice_action.rs:176` `interpret_with_brain` — a single `structured()` classification of free-form text into one fixed intent. A classifier, not an agent.

**The brain seam (single-shot only — no tool loop anywhere yet):**
- `reason.rs:302-313` `trait LocalReasoner` = `id()` + `reason(system,user)->String` + `structured(system,user,schema)->Value`. Three impls: `StubReasoner` (`:375`, deterministic floor), `CloudReasoner` (`:425`, **default**; every call → `make_provider` → fail-closed `cloud_egress_consented` gate + `RedactingProvider`; load-bearing invariant doc-comment `:408-424`), `MistralReasoner` (`reason/mistral.rs:45`).
- **Both cloud and local already converged on schema-in-prompt + `parse_first_json` for `structured()`** — because `Constraint::JsonSchema` *overflowed Bielik-11B's 32K context* on a real Mac (`reason/mistral.rs:133-146`). This is the single most important local-path constraint and it pre-decides "don't depend on native constrained tool-calling."
- `active_reasoner(config)` (`reason.rs:257`) dispatches on `BrainBackend` (Cloud default), resolved once into `AppState.reasoner: Box<dyn LocalReasoner>` (`state.rs:116,179`).

**The gated tool registry (the ready-made tool layer):**
- `tools.rs:34` `enum ToolCall` — 8 variants. **`NoteAside`/`CreateReminder` are commented "Phase-E extension points" (`:30-32`), NOT yet in the enum.**
- `tools.rs:69` `execute_tool(call, db, unlocked: &HashSet<String>, config)` — the ONE synchronous, **egress-free, visibility-gated** seam (`unlocked` non-optional = structural gate; "no constructor lets a caller skip the gate", `:9-14`). `WebSearch`/`CalendarLookup` are refused synchronously (`:180,191`) and dispatched only via async `execute_web_search` (`:224`) / `execute_calendar_search` (`:260`), which are themselves consent-gated/redacted at the connector boundary.
- `mcp.rs:230` `tools_spec()` already emits JSON-schema tool specs (name + description + inputSchema) for the 6 read tools, and `dispatch_tool()` (`:304`) maps name→`ToolCall`→`execute_tool`. **This is the existing model-facing tool catalog to lift into `tools.rs` and reuse.**

**The single-round ancestor of the loop (already merged):**
- `orchestrate.rs:112-160` `orchestrate_context` — asks the model for ONE retrieval *plan* via `structured()`, maps each `{tool,query}` via `map_to_tool_call` (`:204`), runs them all through the gate, and **falls through byte-identically to the deterministic floor when `reasoner.id()=="stub"`** (`:123`). This is the template for the whole design and the `max_steps=1` degenerate case.

**Live integration points + persistence:**
- `transcribe/live.rs:286` `spawn_dispatch` (wake), `:378` `spawn_command_dispatch` (manual button) — both detached OS threads, then `persist_interaction` (`:342` → `insert_assistant_interaction`) + emit `EVENT_VOICE_ACTION_RESULT` (`events.rs:35`). Multi-second answers are already fine off-thread.
- FE: `AssistantStore` (`src/app/core/assistant.store.ts`) — listen-once, capped newest-first `interactions`, pure-computed 4-state orb (`:136-142`, no NG0600), optimistic pending row on wake (`:223-281`). `AssistantActionsComponent` renders it (`assistant-actions.component.ts:45-95`). **Nothing streams; there is no text-ask command and no `Channel` import** (grep-clean). The separate `ask_vault` page (`commands.rs:1423`) does its own retrieval and does NOT touch the reasoner/tool seam.

---

## Findings (per angle)

### Angle 1 — Cloud Claude orchestration: native API vs CLI+MCP
- The Anthropic Messages API tool-use loop is standard: define `tools` (name/description/`input_schema`, optional `strict`, `cache_control`), model returns `stop_reason:"tool_use"` with one or more `tool_use` blocks (parallel tool use exists), reply with `tool_result` blocks, loop to `end_turn`. `tool_choice` = auto/any/tool/none. [overview](https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview), [define-tools](https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools). **(High.)**
- **The Messages API "MCP connector" runs on Anthropic's infra and needs a public URL — it cannot reach `127.0.0.1:8765`.** Native client-tool definitions are the path, not MCP-connector. **(High.)**
- The `claude` CLI headless flags all exist (`-p`, `--output-format stream-json`, `--mcp-config` HTTP w/ bearer, `--allowedTools mcp__murmur__*`, `--max-turns`, `--append-system-prompt`, `--bare`); tool naming is `mcp__<server>__<tool>`. [headless](https://code.claude.com/docs/en/headless), [mcp](https://code.claude.com/docs/en/mcp). **But three blockers:** (a) **redaction bypass** (see Angle 5 — the dealbreaker); (b) our `mcp.rs` is a bare POST/JSON responder, not Streamable-HTTP (no SSE/session-id) — likely needs server work; (c) opaque loop control + cold-start latency. **(Bypass: high. Transport: med-low — needs a spike.)**
- **Sonnet 4.6** (`claude-sonnet-4-6`, released 2026-02-17, **$3/$15 per MTok**) is the right live model (Opus 4.8 is `anthropic.rs`'s current default and costs more). **Prompt caching** on the tools+system prefix (5-min TTL, 0.1× read) is the key live-latency lever; realistic 2–3 hop answer ≈ **3–8 s** with streaming. [sonnet-4-6 news](https://www.anthropic.com/news/claude-sonnet-4-6), [prompt-caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching). **(High; pricing point-in-time. Live latency = real-Mac bar.)**
- Note `anthropic.rs:137-144` sends a single message with **no `tools` param** today — adding the loop is a contained extension of that one file.

### Angle 2 — Local mistral.rs as orchestrator
- **mistral.rs 0.8.1 (already in our lockfile, `Cargo.toml:56`) ships a complete tool stack** (verified in vendored source): `Tool`/`ToolChoice`/`ToolCallResponse` re-exported (`mistralrs-0.8.1/src/lib.rs:279-282`), a per-model tool-call parser for Llama/Qwen-Hermes/Mistral/DeepSeek/Gemma + bare-JSON (`mistralrs-core-0.8.1/src/tools/mod.rs:259-323`), a `ToolCallingMatcher` built for **every** request incl. GGUF (`engine/add_request.rs:145`), and a built-in `Agent` loop (`agent.rs:497`). **(High — traced end-to-end.)**
- **But reliability is the risk, not capability.** Native tool-call delimiters are unconstrained-decode (fragile at Q4 — our registry is all Q4_K_M); GGUF chat templates can silently drop tools if they lack a `tool_use` block (`chat_template.rs:450-454`); **Bielik-11B's GGUF tool template + emitted delimiter are unproven** (its docs show tool-use only via vLLM custom template). Qwen2.5/Qwen3 are the strong path (native `<tool_call>` format), but Qwen3-14B is a *thinking* model (`<think>` can leak around calls). [llama.cpp function-calling](https://github.com/ggml-org/llama.cpp/blob/master/docs/function-calling.md), [quant sensitivity](https://netclaw.dev/troubleshooting/llama-cpp/), [Bielik HF](https://huggingface.co/speakleash/Bielik-11B-v3.0-Instruct). **(Med-high.)**
- **Latency:** 14B Q4 ≈ 18–40 tok/s → a 3-round loop ≈ **10–25 s** (too slow for live voice); 3B ≈ 5–8 s; plus one-time multi-GB model load. → **voice = 1 round on a small model**; note = ≤2–3. **(Med — proxy benchmarks; real-Mac bar.)**
- **Do NOT adopt mistral.rs `Agent` wholesale** — its loop runs tool callbacks on its own threads, *outside* our `execute_tool(...,unlocked,...)` gate (an ungated read path). Hand-roll the ~40-line loop over `set_tools`/`send_chat_request`/`choice.message.tool_calls` → our gated `execute_tool`. **(High.)**
- For small/PL models, a **tiny grammar-constrained tool-router** (`Constraint::Regex`/`Lark` over the tool-name enum) is the deterministic alternative to fragile native tool tokens — and is small enough to avoid the llguidance 32K overflow that the full JSON-schema hit.

### Angle 3 — Unified seam + migration (the design)
- **Build the loop on `structured()`, not native tool-calling.** It's the one primitive all three impls already share (schema-in-prompt + `parse_first_json`), so the loop is transport-agnostic with zero new per-impl code; native tool-calling becomes an *optional later override* of `agentic()` for the local impl, not a dependency. **(High — direct extrapolation of the shipped `structured()` decision.)**
- Concrete seam (full sketch in the agent brief): a new `agent.rs::run_agentic_loop(reasoner, system, user, executor, max_steps)`; a `ToolExecutor` trait with `specs()` (the per-caller allowlist) + `run(name,args)` (gated); a `GatedToolExecutor{ db, unlocked, config, meeting_id, app, allow_writes }` shared by cloud/local/voice/text (and a read-only subset for MCP); a default `LocalReasoner::agentic()` that delegates to the loop.
- **Tool catalog the model sees** (lift `mcp.rs::tools_spec` into `tools.rs` as the single source): `search_meetings`, `search_semantic`, `get_meeting`, `list_recent_meetings`, `get_open_commitments`, `get_entity_dossier` (always), `web_search` (only when app present + enabled+consented+keyed — else omitted from `specs()`), `calendar_lookup` (app present), plus **new** `note_aside` / `create_reminder` (only when `allow_writes` + a recording is active; uncomment `tools.rs:30-32` + 2 arms in `execute_tool`).
- **Migration = 4 additive, flag-gated phases** (`agentic_voice`, default off), removing nothing hard until proven on a real Mac:
  - **P0 (headless, zero behavior change):** add the types/executor/loop/default method; lift `tool_specs()`; implement the two write tools (kept out of the MCP allowlist); unit-test with Stub/Mock reasoners + the seeded visible/sealed fixture — **RED-before-GREEN that sealed content never surfaces through the loop**, citations extracted, `max_steps` bounded, panic-free. Lock-security-review the new executor + write tool.
  - **P1 (dual path, opt-in):** flag on → `GatedToolExecutor` + `reasoner.agentic()` → map `AgentOutcome`→`VoiceActionResult` (same DTO/persist/event, FE unchanged); flag off → today's path verbatim.
  - **P2 (text input, same entry):** extract `run_assistant_turn(app, command: String)` that both `spawn_command_dispatch` (voice→String) and a new `ask_assistant_text(text)` command call. Identical executor/loop/persist/event → one card for voice + text.
  - **P3 (retire hardcode to FLOOR):** default flag on; demote `parse_voice_intent`/`handle_voice_action`/`rag_answer` to the stub/no-consent fallback; `interpret_with_brain` is subsumed by the loop's first turn. `VoiceIntent`/`detect_wake` stay (wake event + acoustic gate).

### Angle 4 — Live latency, streaming UX, text integration
- **Parallelize tools, cap rounds.** Anthropic: serial sub-searches were "painfully slow"; parallel tools "transform speed" ([multi-agent system](https://www.anthropic.com/engineering/multi-agent-research-system)); "each autonomous turn adds latency" → cap at 2–3 ([building effective agents](https://www.anthropic.com/research/building-effective-agents)). Our current `for call in &tool_calls` (`voice_action.rs:302`) is the serial anti-pattern; the vault tools are sync DB reads, web/calendar already run on scoped blocking threads → `join` them.
- **Stream via a Tauri `Channel<AssistantDelta>`, not events** (events are unordered/slow under rapid succession; Channels are ordered/fast, used internally for child-process streaming — [Tauri docs](https://v2.tauri.app/develop/calling-frontend/)). One channel registered once at `init()` (so it serves the backend-initiated wake path too). Phases: `Started{interactionId,source,command}` → `Thinking` → `Tool{tool,state:running|done,count}` → `Token{text}` → `Done{status,citations}` → `Error`. This **subsumes** `EVENT_VOICE_COMMAND_PROCESSING` + `EVENT_VOICE_ACTION_RESULT`.
- **The hold-back buffer (the one hard part):** `redact.rs` `restore()` is a whole-string replace over `⟪NAME_n⟫`/regex tokens (`:188,232,356`). Streaming a *cloud* answer token-by-token can split a placeholder → only flush the prefix up to the last byte that cannot begin an open token; restore that safe prefix; carry the tail. Trace phases carry no PII and stream freely; **local providers bypass redaction and stream raw.**
- **FE extension (signals-first, rules-compliant):** `AssistantInteraction` gains `source: voice|wake|text`, `trace: ToolTraceStep[]`, `streaming: boolean`, and `summary` accretes `Token` deltas. `channel.onmessage = onDelta` (a listen-once callback, NOT inside a tracked `effect()` → no NG0600, mirrors today's `onResult`). Auto-scroll via `afterNextRender(fn,{injector})`. Per-tool **chips** (icon + running-dots/✓+count), web chip visually distinct ("via web" = the loud egress disclosure). Text composer in the assistant-card head **optimistically prepends** a pending row (the `onWake` pattern) reconciled by `interactionId`. Voice/wake/text = one timeline, one orb, one trace renderer.
- **Market:** Granola = the bar (live chat + double-click-to-verify inline citations); Cluely = the 300 ms latency expectation **and** the privacy anti-pattern (2025 breach of 83k transcripts); Perplexity = the plan→execute→observe trace to copy; MS/Google copilots are summary/action-items, not live agentic Q&A. **Murmur's differentiator = the same live agentic Q&A but local-first with visible, gated/on-device sources.** [Granola](https://www.granola.ai/blog/chat-with-meetings-search-analyze-ai-2026), [Cluely review](https://tldv.io/blog/cluely-review/), [Perplexity](https://www.langchain.com/breakoutagents/perplexity).

### Angle 5 — Security / gating (the adversarial veto)
- **Gating-via-`execute_tool` is architecturally sufficient against "model asks for sealed content"**: the model emits only tool name+args strings; `unlocked` is a host-held `&HashSet` the model can't supply or mutate; no tool mutates visibility; one chokepoint with no ungated overload. *Enforcement rule:* build the executor with the live `unlocked`, pass it to every `execute_tool`; never add an unlock/seal/export/destructive tool; args are strings only. **(High.)**
- **THE VETO — Route A (`claude` CLI + our MCP) bypasses the redaction firewall.** In an MCP agent loop the CLI uploads every `tool_result` (raw gated meeting content — `get_meeting` returns cleartext `NOTE:…TRANSCRIPT:…`, `tools.rs:124-128`) to Anthropic so the model can pick the next step; `RedactingProvider` only wraps the `complete`/`summarize` we issue and has **zero visibility** into MCP-fetched content. It also forces removing the hermetic `--disallowedTools` seal (`claude_code.rs:40-51`). A redacting MCP proxy can't save it (breaks the local no-egress promise; no way to restore tokens since the CLI owns the reply). **Veto Route A as a Murmur-initiated cloud path.** (The MCP server stays valid for the *user's own* Claude Desktop — the user's egress decision through their client.) [Anthropic: "tool output is an attack surface even when trusted"](https://www.anthropic.com/engineering/how-we-contain-claude), [first-party MCP injection bugs](https://www.theregister.com/2026/01/20/anthropic_prompt_injection_flaws/). **(High on bypass.)**
- **Route B (the only safe model-driven cloud loop):** `anthropic` Messages API where WE own the loop and **redact every `tool_result` before egress.** Needs a net-new **loop-scoped redactor** (today's one-shot `complete` redaction with a fresh per-call map doesn't carry a consistent token map across turns). **NB — this only matters for a *native-API* loop; the recommended `structured()`-based loop sidesteps it entirely** because every turn re-routes the whole accumulated prompt through `make_provider`→`RedactingProvider`, so each egressing turn is already redacted (token-map consistency across turns is then a quality nicety, not a security gap).
- **Write-actions safely:** the transcript is *other people's untrusted speech* → prompt-injection risk (OWASP LLM01 + LLM06 Excessive Agency). Contain it: (1) the reader loop over untrusted content is **read-only**; (2) the `ToolCall` enum must never gain a destructive tool — `NoteAside`/`CreateReminder` are append-only + `meeting_is_visible`-gated; (3) a **user-dictated** write is auto-OK, but a **model-PROPOSED** write must surface as a **human-confirmed proposal** ("Claude proposes reminder 'X' — from [[Meeting]]"), never auto-execute; (4) no exfiltration tools (the only outbound is the redacted, consent-gated web *search*). [OWASP LLM Top-10 2025](https://owasp.org/www-project-top-10-for-large-language-model-applications/assets/PDF/OWASP-Top-10-for-LLMs-v2025.pdf). **(High.)**
- **Consent in the loop = hide AND sentinel:** omit unavailable tools from the advertised `specs()` each turn (least-privilege — the model can't call what it can't see), keep the fail-closed sentinel as the second line, and **rebuild exposure per turn** so a mid-loop consent revocation immediately removes the tool. **(High.)**

---

## Fit with Murmur's constraints

- **Local-first / privacy:** Recommended design routes every cloud turn through `make_provider`→consent gate + `RedactingProvider` → **same egress class as today's single grounding blob**, just N turns of it (cap `max_steps` + budget re-fed results to bound amplification; flag the `NoopNameRedactor` default loudly). Local loop egresses nothing. ✅ Route A would have violated this — vetoed.
- **SQLite-canonical / lock-model:** the loop is a thin reader over the gated `execute_tool`; no new ungated path, no new source of truth (final answer still lands via `insert_assistant_interaction`). The new write tool + executor are a **required `lock-security-reviewer` gate**. ✅
- **Provider seam + redaction firewall:** extended additively (new default trait method); cloud rides the existing envelope unchanged. The default `claude_code` provider **can** participate (its `structured()` answers the decide-or-finish prompt each turn; we run the tools) — so unlike a native-API-only design, there is **no default-provider gap**. ✅
- **macOS / CI honesty:** loop mechanics, gating, citation, bounding, and the hold-back buffer are all headless-testable with mock reasoners + seeded gated fixtures. **Real-Mac-only:** local multi-step tool-call reliability per GGUF, Polish quality, live latency feel, `claude stream-json` schema, and any FFI/permission behavior. `cargo test --lib` is never proof of those.
- **No new heavy deps:** the loop is `std` + `serde_json`; mistral.rs is already linked (no version bump needed for 0.8.1's stack). FE adds no npm packages (`@tauri-apps/api` already has `Channel`). ✅

---

## Options & tradeoffs

| Option | What | Effort | Risk | Verdict |
|---|---|---|---|---|
| **A. Default `agentic()` over `structured()` + `GatedToolExecutor`** | One shared bounded loop, both backends free, hardcode kept as floor, text + voice unified, streaming via Channel | **M** | Low (additive; mirrors merged Flow A) | **RECOMMENDED** |
| B. Required `agentic()` per impl | Each reasoner implements its own loop | L | Med (breaks 3 impls + ~15 tests, no shared floor) | Reject |
| C. `claude -p` drives its own loop vs our MCP | Cheapest agent trace for the default provider | M | **Unacceptable** (redaction bypass; unseals hermetic CLI) | **Veto** |
| D. Native vendor tool-calling (anthropic `tool_use` loop / mistral.rs `Agent`) | First-class parallel tools, strict schema, prompt caching | M | Med (anthropic = needs loop-scoped redactor; local native = unproven on Q4/PL GGUFs) | **Defer** — optional override of A after A ships |
| E. Trace-only streaming first | Channel + tool-trace deltas, answer still one block | S | Lowest (no hold-back needed) | **Good first slice within A** |

---

## Recommendation & first step

**Build Option A, sliced for de-risking.** The smallest verifiable first step is **P0 + two RED-before-GREEN spikes** that lock the load-bearing risks before any behavior changes:

1. **Headless P0 (no behavior change):** land `ToolSpec`/`ToolExecutor`/`AgentOutcome`/`AgentStep` + `GatedToolExecutor` + `run_agentic_loop` + the default `LocalReasoner::agentic()`; lift `tool_specs()` into `tools.rs` (mcp.rs derives its JSON from it — its 6 tests stay green); uncomment + implement `NoteAside`/`CreateReminder` in `execute_tool` (out of the MCP allowlist). Wire nothing into dispatch yet.
2. **Security spike (RED-first):** with a `MockReasoner` that asks for `get_meeting` on a **sealed** meeting then "answers", assert the loop surfaces **nothing** from it (mirrors `voice_action.rs`/`orchestrate.rs` gated tests) — fails on a naive loop, passes once the executor binds `unlocked`. Add a test asserting **no `ToolCall` variant can mutate `unlocked` or do a destructive write.** Route the whole change through `lock-security-reviewer`.
3. **Local-reliability spike (real Mac, the only thing that decides local):** with each registry GGUF present, run one `run_agentic_loop` over 5–10 PL+EN commands and measure whether the model reliably emits clean `{tool|answer}` step JSON across 1–3 turns + the wall-clock. If flaky → cap local `max_steps=1` and lean on the deterministic floor (zero regression either way); if a grammar-router is needed, confirm a *tiny* `Constraint::Regex` avoids the 32K llguidance overflow.

Then P1 (dual-path behind `agentic_voice`, FE unchanged), P2 (the text composer + `ask_assistant_text` sharing `run_assistant_turn`), the streaming Channel refactor (start trace-only, then token streaming with the hold-back buffer), and finally P3 (default the flag on, demote the hardcode to floor).

---

## Open questions / what couldn't be verified

- **Local multi-step tool-call reliability** on our Q4 GGUFs (esp. Bielik's GGUF chat template + emitted delimiter) — real-Mac-only; the load-bearing local risk. May force a Q5/Q6 registry entry or `max_steps=1` for local.
- **Live latency** of an N-hop loop (cloud and local) on a real Mac — unmeasured; target <8 s; the honest bar is a recorded round-trip.
- **`claude stream-json` exact message schema** is officially under-documented — needs a live capture before relying on it (only matters if we ever revisit a CLI fast-path, which we're not for the seam).
- **mistral.rs `chat_stream` fit** through our synchronous `MistralReasoner` wrapper — API exists; the wrapper conversion needs a spike.
- **Cross-turn redaction token-map consistency + name-redaction amplification** (`NoopNameRedactor` default) — a quality/disclosure issue to tune on a live DB, not a security gap for the `structured()`-based loop.
- **Whether to also fold Flow A (note grounding) onto the same loop** — architecturally clean (it's the single-shot ancestor) but out of scope here; a follow-up once Flow B is proven.

---

## Sources

**Code (this repo, verified this session):** `reason.rs:257,302-313,375,408-424,425-523`; `reason/mistral.rs:45,118-147,133-146`; `tools.rs:9-14,30-32,34,69-204,224-277`; `voice_action.rs:108-161,176-221,257-509,514-537`; `audio/wake.rs:255-268,273`; `orchestrate.rs:112-160,204-213,123`; `transcribe/live.rs:286,342,378,399-411,426`; `summarize/provider.rs:42-55`; `summarize/anthropic.rs:125-247`; `summarize/claude_code.rs:40-51,249-404`; `summarize/mod.rs:63-128`; `summarize/redact.rs:5-6,188,232,305-358`; `mcp.rs:75-346`; `connectors/mod.rs:141-174`; `state.rs:116,179`; `events.rs:35,43,59`; `src/app/core/assistant.store.ts:32-78,136-142,223-281`; `src/app/features/record/assistant-actions.component.ts:45-95`; `src/app/core/ipc.service.ts:295,511,523,615-653`. Vendored: `mistralrs-0.8.1/src/{lib.rs:279-282,agent.rs:497}`, `mistralrs-core-0.8.1/src/{tools/mod.rs:259-323,engine/add_request.rs:145,pipeline/chat_template.rs:450-454,request.rs:21-27}`.

**External (point-in-time, fetched 2026-06-30):**
- Anthropic tool use: https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview · https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools
- Prompt caching: https://platform.claude.com/docs/en/build-with-claude/prompt-caching
- Streaming: https://platform.claude.com/docs/en/build-with-claude/streaming · https://docs.anthropic.com/en/api/messages-streaming
- Claude Code headless/CLI/MCP: https://code.claude.com/docs/en/headless · https://code.claude.com/docs/en/cli-reference · https://code.claude.com/docs/en/mcp
- Sonnet 4.6: https://www.anthropic.com/news/claude-sonnet-4-6
- Agent design: https://www.anthropic.com/research/building-effective-agents · https://www.anthropic.com/engineering/multi-agent-research-system
- Security: https://www.anthropic.com/engineering/how-we-contain-claude · https://genai.owasp.org/llmrisk/llm01-prompt-injection/ · https://owasp.org/www-project-top-10-for-large-language-model-applications/assets/PDF/OWASP-Top-10-for-LLMs-v2025.pdf · https://www.theregister.com/2026/01/20/anthropic_prompt_injection_flaws/ · https://labs.cloudsecurityalliance.org/agentic/agentic-mcp-security-best-practices-v1/
- mistral.rs: https://github.com/EricLBuehler/mistral.rs · https://docs.rs/mistralrs/latest/mistralrs/
- Local model/quant/latency: https://github.com/ggml-org/llama.cpp/blob/master/docs/function-calling.md · https://netclaw.dev/troubleshooting/llama-cpp/ · https://huggingface.co/speakleash/Bielik-11B-v3.0-Instruct · https://markaicode.com/benchmarks/hugging-face-qwen-3-m4-max-throughput-benchmark/
- Market: https://www.granola.ai/blog/chat-with-meetings-search-analyze-ai-2026 · https://tldv.io/blog/cluely-review/ · https://otter.ai/chat · https://www.langchain.com/breakoutagents/perplexity

**Prior Murmur docs built on (not repeated):** `docs/DESIGN-local-brain-orchestration.md`, `docs/PLAN-brain2-rag-voice.md`, `docs/superpowers/specs/2026-06-26-brain2-voice-design.md`, `docs/research/2026-06-28-local-model-voice-decision.md`, `docs/research/2026-06-28-ai-assistant-orb-ui.md`, `docs/research/2026-06-29-murmur-deep-analysis-context-engines.md`.
