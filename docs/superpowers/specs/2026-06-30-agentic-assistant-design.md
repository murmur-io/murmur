# Design spec — Agentic in-meeting assistant (voice + text), model-driven, zero hardcoded routing

**Date:** 2026-06-30 · **Status:** draft for review · **Research basis:** `docs/research/2026-06-30-agentic-brain-tool-use-rebuild.md` (5-angle fan-out) · **Originating ask:** add a TEXT way to ask the in-meeting assistant alongside VOICE — which surfaced that the decision engine must stop being a hardcoded keyword router and become a model-driven agentic tool-use loop.

---

## 1. Goal & non-goals

**Goal.** Replace the hardcoded intent router (`parse_voice_intent` + `handle_voice_action` + `rag_answer`) with a **model-driven, bounded tool-use loop**: the brain (cloud Claude *or* local mistral.rs) decides which gated tools to call and when to answer — **zero hardcoded "what to do" logic**. Both **voice** (wake/click → transcribed text) and a **new text composer** feed the *same* loop. Add production-grade streaming status/animation (tool trace + answer).

**Non-goals (this spec).**
- Native vendor tool-calling as the foundation (Anthropic `tool_use` blocks / mistral.rs `Agent`) — deferred as an optional optimization (§9).
- Converging the note-grounding path (`orchestrate.rs` Flow A) onto the loop — follow-up.
- The `claude` CLI → local-MCP route — **vetoed** (§7, redaction bypass).
- Token-level answer streaming with the redaction hold-back buffer — phased in *after* trace-only streaming (§6, §8).

**Decisions taken (please confirm at review).**
1. **D1 — Loop is built on the existing `structured()` primitive**, not native tool-calling. Rationale: every reasoner already implements `structured()` (schema-in-prompt + `parse_first_json`); the loop is then transport-agnostic with zero per-impl code, **and every cloud turn auto-routes through `make_provider`→`RedactingProvider`**, so redaction is preserved for free (the security win, §7).
2. **D2 — v1 agentic loop is READ-ONLY.** Exposed tools = the 6 gated readers + (when available) web/calendar. Write actions (`note_aside`/`create_reminder`) are **defined** but **not** in the autonomous toolset yet; they stay on the existing *user-dictated* path until the model-proposed-write confirmation UX lands (§7, a later phase). This follows the "reader tier handling untrusted transcript = read-only" rule.
3. **D3 — Local is bounded.** Voice `max_steps = 1` (decide → parallel-execute → synthesize); the loop shape is identical to cloud, only the budget differs. The existing single-round `orchestrate.rs` plan is the `max_steps = 1` ancestor and remains the **deterministic FLOOR** (stub / no-consent fallback).
4. **D4 — Streaming starts trace-only.** First slice streams the *tool trace* (Thinking / Tool running→done+count) via a Tauri `Channel`; the answer still arrives as one block (like today). Token streaming + the redaction hold-back buffer is a fast-follow, not v1.
5. **D5 — Hardcode is demoted, never hard-deleted.** `parse_voice_intent`/`handle_voice_action`/`rag_answer` become the floor; `VoiceIntent`/`detect_wake` stay (wake event + acoustic gate). Migration is flag-gated (`agentic_voice`, default off), dual-path, rollback = flag flip.
6. **D6 — Cloud agentic model = Sonnet 4.6** (`claude-sonnet-4-6`) for latency/cost over the Opus 4.8 default; surfaced as the assistant model, not forced. (The default `claude_code` CLI provider participates too — it answers the per-turn decide-or-finish `structured()` prompt; we run the tools.)

---

## 1a. Verification corrections (BINDING — 2026-06-30, from the adversarial workflow)

The verify-agentic-brain workflow (`docs/research/2026-06-30-agentic-brain-verification-verdict.md`) confirmed the architecture but found **one root defect** (the floor was mis-defined) that, on the **shipped default** (`Cloud` + `cloud_egress_consented:false` + `claude_code`), regresses nearly every user. These corrections **override** the decisions above where they conflict and are binding for the plan:

- **C1 — THE FLOOR IS THE DETERMINISTIC PATH, not `reason()`-over-gathered.** On non-convergence / `structured()` `Err` / stub, `run_assistant_turn` MUST fall through to `resolve_command_intent` → `handle_voice_action`/`rag_answer` (the gated fan-out + cited synthesis + `needs_consent`). This makes D5's "demote, never delete" literally true and keeps `rag_answer` load-bearing instead of dead code behind an always-on flag. (Replaces the §3.2 `reasoner.reason(SYNTH, gathered)` floor.)
- **C2 — WRITES ARE DECOUPLED FROM THE FLAG.** The entry classifies write-vs-informational (a legitimate routing decision — NOT "the model picks a read tool", so it doesn't betray zero-hardcoded-routing for the part that matters): an explicitly **user-dictated** `CreateReminder`/`NoteAside` goes to `handle_voice_action` (today's path, auto-OK); only **informational** turns enter the loop. Without this, P3 silently kills voice reminders/asides (the project's #1 failure mode). Amends **D2**.
- **C3 — PROPAGATE `AppError::Unavailable`** out of the loop (no blanket `Err(_) => break`), so the mapper can emit `status:needs_consent` + the gated citations the floor gathered, exactly as `rag_answer` does today.
- **C4 — LOCAL DEFAULTS TO THE DETERMINISTIC FAN-OUT.** Do NOT route local through `reasoner.agentic()` at v1. "Local agentic" is gated behind the real-Mac spike and turned on only if a GGUF is shown to (a) emit clean `{tool,args}` at Q4 AND (b) beat the fan-out's answer QUALITY on ~15–20 PL+EN commands. Amends **D3** (stop overselling "local agentic live").
- **C5 — DO NOT DEFAULT `agentic_voice` ON IN P3** until C1 is done AND the spike passes a **measured latency + answer-quality budget** (per our own prove-before-swap prior, `DESIGN-local-brain-orchestration.md:9,54`). Amends **D5** / §9 P3.
- **C6 — RE-SNAPSHOT `unlocked` PER TURN** (not once at loop start) — a mid-loop screen-share auto-relock leaves the snapshot stale across a 10–25s loop. Rebuild it each turn alongside the `specs()` rebuild. Adds to §7.
- **C7 — GUARDRAILS ARE BINDING.** `max_steps=1` for voice, deterministic floor, structured-not-native, read-only v1 are the exact mitigations our priors demanded for the "agentic-RAG-overkill trap" — if any is relaxed (esp. multi-round-by-default on local) the design crosses into the trap. Adds to §1 non-goals.
- **C8 — LATENCY POSITIONING.** Frame as a "fast grounded cited assistant" (~p50<10s / p95<18s cloud), NOT a Cluely-300ms overlay; **parallelize the floor's serial fan-out** (`voice_action.rs:302` `join()`), and **pull streaming-synthesis into v1** (`claude -p --output-format stream-json` through the redaction hold-back) — the token-by-token final pass is the live-feel lever; trace-only leaves the worst half frozen. Amends **D4/D5** sequencing.
- **C9 — DURABLE TRACE OR DROP THE CLAIM.** The `AgentStep` trace is durable in NONE of the 4 phases as drawn. Either add an additive, `meeting_id`-scoped, visibility-gated, **purged-on-seal** trace store (lock-reviewed exactly like the prior 🔴 `correction_log` flywheel fix, `DESIGN:26-38`) or delete the "for persistence / the flywheel" contract from `AgentStep`.

**New required P0 tests (RED-first):** (a) `structured()` → `Err(Unavailable)` yields gated citations + `needs_consent`; (b) stub-floor ≡ `rag_answer` (mirror `orchestrate.rs:389`); (c) a user-dictated reminder/aside still fires through the agentic path; (d) folder relocked mid-loop → subsequent tool calls surface nothing.

---

## 1b. As-built deviations (2026-06-30 — trust code, not this doc)

The implementation landed with two deliberate, cleaner-than-spec deviations (verified fully wired, zero dead refs, `clippy -D warnings` clean):

- **§3.3 — no `LocalReasoner::agentic()` trait method.** The loop ships as the FREE FUNCTION `crate::agent::run_agentic_loop(&dyn LocalReasoner, …) -> Result<Option<AgentOutcome>>`, called directly from `run_informational`. A default trait method would hit an object-safety conflict (coercing `&Self` → `&dyn LocalReasoner` needs `Self: Sized`, which would make the method non-dispatchable on the `Box<dyn LocalReasoner>` in `AppState`). The free function is the clean, no-hack equivalent.
- **§3.5 — live trace ships via a Tauri EVENT, not a `Channel<AssistantDelta>`.** The tool trace is a low-frequency stream (a handful of events per turn), so `EVENT_ASSISTANT_TOOL` + `AssistantToolPayload {tool, state, ok, count}` (events.rs) is the right primitive; the `Started/Thinking/Tool/Done` phases collapse onto the existing `EVENT_VOICE_COMMAND_PROCESSING` + `EVENT_VOICE_ACTION_RESULT` + the new tool event. The **`Token` (token-by-token answer streaming)** variant is **deliberately DEFERRED** (it's the verdict's one OPTIONAL improvement — it needs a provider `complete_streaming` + the redaction hold-back buffer; risky to rush). The answer renders complete with the live tool-trace shown during.

Everything else matches §2–§9 + the C1–C9 corrections.

---

## 2. Architecture overview

```
            ┌─────────────── one entry point ───────────────┐
  voice ───►│  run_assistant_turn(app, command: String)     │◄─── text (new composer)
 (wake/     └───────────────────────┬───────────────────────┘
  click→ASR)                        │ builds
                                    ▼
                      GatedToolExecutor { db, unlocked, config, meeting_id, app, allow_writes }
                                    │ specs() = per-caller allowlist (gated/consent-aware)
                                    ▼
        reasoner.agentic(system, command, executor, max_steps) ──► AgentOutcome { answer, steps, citations }
                                    │ default impl = run_agentic_loop() over structured()
        ┌───────────────────────────┼───────────────────────────┐
        ▼ (each turn)               ▼                            ▼
   CloudReasoner.structured()   MistralReasoner.structured()   StubReasoner (floor)
     → make_provider →            → in-process GGUF             → deterministic
       consent gate +               (no egress)                   fan-out (today's
       RedactingProvider                                          rag_answer logic)
        │                                                         │
        └──────────► tool calls execute via executor.run() ──► tools::execute_tool (GATED)
                                    │ emits phased deltas
                                    ▼
                      Tauri Channel<AssistantDelta> ──► AssistantStore (signals) ──► AssistantActionsComponent
```

**One brain, two inputs, one timeline.** Voice and text differ only by an interaction `source` tag; both produce a `String` command and call `run_assistant_turn`. Answers land in the same newest-first `interactions` list with the same orb / citations / status rendering.

---

## 3. Backend components (Rust, `src-tauri/src/`)

### 3.1 `tools.rs` — lift the model-facing catalog + add the gated executor
- **`ToolSpec`** (new): `{ name, description, parameters: serde_json::Value (JSON-schema object), egress: EgressClass, write: bool }`. `tool_specs() -> &'static [ToolSpec]` becomes the single source; `mcp.rs::tools_spec()` derives its JSON from it (its 6 tests stay green).
- **`trait ToolExecutor: Send + Sync`** (new): `fn specs(&self) -> &[ToolSpec]` (the allowlist this caller exposes) + `fn run(&self, name: &str, args: &serde_json::Value) -> Result<String>` (gated + egress-aware).
- **`struct GatedToolExecutor<'a>`** (new): holds `db: &Db`, `unlocked: &HashSet<String>`, `config: &AppConfig`, `meeting_id: &str`, `app: Option<&AppHandle>`, `allow_writes: bool`. `specs()` = `tool_specs()` filtered by `app.is_some()` (web/calendar) + `allow_writes` (actions). `run()` re-validates the name against `specs()` (refuse anything not advertised), then routes: sync readers → `execute_tool`; `web_search`/`calendar_lookup` → `block_on(execute_web_search/execute_calendar_search)` (only when `app` present); write tools → gated helpers (deferred per D2).
- **`enum ToolCall`**: uncomment `NoteAside`/`CreateReminder` (`tools.rs:30-32`) + add two `execute_tool` arms (NoteAside reuses `voice_action.rs:514` `meeting_is_visible`-gated logic; CreateReminder reuses `add_reminder_blocking`). Kept OUT of the MCP allowlist (`write: true`) and out of the v1 autonomous set.

**Invariant (load-bearing):** the model emits only `{tool, args}` strings; `run()` binds the host-held `unlocked` and routes through the existing gate, so the model can request a search but **cannot bypass `visibility_clause`** and **cannot mutate visibility** (no unlock/seal/export/destructive tool exists). Gating is enforced by the executor's construction, not by trusting the model.

### 3.2 `agent.rs` (new) — the shared bounded loop
```rust
pub struct AgentOutcome { pub answer: String, pub steps: Vec<AgentStep>, pub citations: Vec<String> }
pub struct AgentStep { pub tool: String, pub args_json: String, pub ok: bool }

pub fn run_agentic_loop(
    reasoner: &dyn LocalReasoner, system: &str, user: &str,
    executor: &dyn ToolExecutor, max_steps: usize,
    sink: Option<&dyn DeltaSink>,         // streaming (trace); None in headless tests
) -> Result<AgentOutcome>;
```
Loop (`std` + `serde_json` only, panic-free, bounded):
1. Compose `agent_system` = `system` + the rendered tool catalog (`executor.specs()`) + the **step protocol**: *"Reply with EITHER `{"tool": <name>, "args": {…}}` to use a tool, OR `{"answer": "…"}` to finish."*
2. `for step in 0..max_steps`: `v = reasoner.structured(agent_system, transcript, STEP_SCHEMA)?`.
   - `v.answer` → finish: `AgentOutcome { answer, steps, citations: extract_citations(gathered) }`.
   - `v.tool` → `sink.tool(name, Running)`; `out = executor.run(name, args)?`; `sink.tool(name, Done, count)`; `gathered += out`; `transcript += truncate(out, RESULT_BUDGET)`; record `AgentStep`.
   - parse failure / no convergence by `max_steps` / `structured()` `Err` → **FLOOR** (per **C1**): the loop returns control to `run_assistant_turn`, which runs the deterministic path `resolve_command_intent` → `handle_voice_action`/`rag_answer` (the gated fan-out + cited synthesis + `needs_consent`) — NOT a bare `reason()` over `gathered`. Stub / no-consent hit the floor too, so the default config behaves byte-for-byte like today.
- **Citations** are extracted from GATED tool output only (lift `voice_action.rs`'s `extract_citations`/web/calendar helpers into a shared module) → `[[Title]]` / `(web)` / `(calendar)`.

### 3.3 `reason.rs` — one additive default trait method
```rust
pub trait LocalReasoner: Send + Sync {
    fn id(&self) -> &str;
    fn reason(&self, system: &str, user: &str) -> Result<String>;
    fn structured(&self, system: &str, user: &str, json_schema: &Value) -> Result<Value>;
    /// NEW — bounded tool-use loop. DEFAULT delegates to run_agentic_loop over structured(),
    /// so Cloud/Mistral/Stub get it identically. Never panics; bounded by max_steps.
    fn agentic(&self, system: &str, user: &str, executor: &dyn ToolExecutor,
               max_steps: usize, sink: Option<&dyn DeltaSink>) -> Result<AgentOutcome> {
        crate::agent::run_agentic_loop(self, system, user, executor, max_steps, sink)
    }
}
```
No impl breaks (default method). A future `MistralReasoner` MAY override `agentic()` with native mistral.rs tool-calling (§9). `CloudReasoner` keeps routing every turn through `make_provider` → consent gate + `RedactingProvider` (no new egress class; same envelope as today's single grounding call, just N turns — cap `max_steps`, budget re-fed results).

### 3.4 `transcribe/live.rs` + `commands.rs` — one entry, two inputs
- Extract **`run_assistant_turn(app, command: String, source: Source)`** that: builds `GatedToolExecutor` from the live `AppState`; **(C2)** if the command is an explicit user-dictated write (`resolve_command_intent` → `CreateReminder`/`NoteAside`) it runs `handle_voice_action` directly (today's path); otherwise it calls `reasoner.agentic(...)`. **(C1/C3)** on `Err(Unavailable)` / non-convergence / stub it falls through to `handle_voice_action`/`rag_answer` (the deterministic floor, which carries `needs_consent` + gated citations). Then maps the result → `VoiceActionResult` (same DTO), `persist_interaction` (unchanged), streams deltas + emits the terminal result. **(C4)** Local backend skips `reasoner.agentic()` entirely at v1 and always runs the deterministic path until the spike promotes it.
- **Voice:** `spawn_command_dispatch` (and the wake path) call `run_assistant_turn(app, command, Voice|Wake)` when `agentic_voice` is ON; OFF → today's `handle_voice_action` verbatim (dual-path).
- **Text (new):** `#[tauri::command] ask_assistant_text(app, state, text: String)` → `run_assistant_turn(app, text, Text)`. Runs only while recording (parity with voice; `meeting_id` from `AppState.current_meeting`). Registered in `lib.rs` `generate_handler!`.

### 3.5 Streaming contract — `events.rs` + a Tauri `Channel`
```rust
#[serde(rename_all = "camelCase", tag = "phase")]
pub enum AssistantDelta {
    Started { interaction_id: u64, source: Source, command: String },  // Source: Voice|Wake|Text
    Thinking { interaction_id: u64, seq: u32 },
    Tool { interaction_id: u64, seq: u32, tool: ToolKind, state: ToolState, count: Option<u32> },
    Token { interaction_id: u64, seq: u32, text: String },  // Phase: token streaming (post-v1)
    Done { interaction_id: u64, status: String, citations: Vec<String> },
    Error { interaction_id: u64, message: String },         // non-PII
}
```
A single `Channel<AssistantDelta>` registered once at FE `init()` (so it serves the backend-initiated **wake** path, which has no `invoke`), stored in `AppState` (`Mutex<Option<Channel>>`); both dispatch paths write to it. It **subsumes** `EVENT_VOICE_COMMAND_PROCESSING` (→ `Thinking`/`Tool`) and `EVENT_VOICE_ACTION_RESULT` (→ `Done`/`Token`). `WakeDetected`/`Listening` stay as-is. v1 emits `Started`/`Thinking`/`Tool`/`Done` (trace-only, D4); `Token` is added with the hold-back buffer later.

---

## 4. Frontend components (Angular 18 zoneless, `src/app/`)

- **`AssistantStore`** (`core/assistant.store.ts`): `AssistantInteraction` gains `source: "voice"|"wake"|"text"`, `trace: ToolTraceStep[]`, `streaming: boolean`; `summary` accretes future `Token` deltas. New `channel.onmessage = (d) => this.onDelta(d)` registered in `init()` — a **listen-once callback, NOT inside a tracked `effect()`** (no NG0600; mirrors today's `onResult`). `onDelta` does pure immutable signal updates (prepend on `Started`, upsert trace chip on `Tool`, resolve on `Done`). `orbState` stays a **pure computed**. New `askText(text)` optimistically **prepends a pending row** with the typed text *before* the await (the `onWake` pattern), reconciled by `interactionId`.
- **`IpcService`** (`core/ipc.service.ts`): add `askAssistantText(text): Promise<void>` (one method per command) + `registerAssistantStream(channel)`. Types in `core/models.ts`.
- **`AssistantActionsComponent`** (`features/record/assistant-actions.component.ts`): add the **tool-trace chip row** under the command — per-tool inline-SVG icon + label; `running` → existing thinking-dots/shimmer; `done` → ✓ + count (e.g. "✓ 142 notatki"); the `web` chip is visually distinct ("via web" = loud egress disclosure, reuse the vault-vs-web split). Resolved rows collapse the trace to the existing source chips. `@if`/`@for track id`, tokens-only styling, `prefers-reduced-motion` honored. Overlays N/A (the card is in-flow `.card`).
- **Text composer** (in the assistant-card head, per the user's earlier UX decision): textarea + send button mirroring `ask.component.ts` (auto-grow, spinner-on-pending). On submit → `store.askText()`. Auto-scroll to newest via `afterNextRender(fn, {injector})` (never `setTimeout`/rAF in a component).

---

## 5. Data flow (one turn, voice or text)

1. Input → `String` command → `run_assistant_turn(app, command, source)`.
2. Emit `Started{interactionId, source, command}` → FE prepends a pending row.
3. `reasoner.agentic(system, command, executor, max_steps, sink)`:
   - each turn: `structured()` → either a `{tool,args}` (emit `Tool{running}` → gated `execute_tool` → emit `Tool{done,count}`) or `{answer}` (finish), bounded by `max_steps`; floor on non-convergence.
4. `AgentOutcome` → `VoiceActionResult` → `persist_interaction` (`insert_assistant_interaction`) → emit `Done{status, citations}` (+ `Token*` once streaming lands).
5. FE resolves the pending row → markdown answer + citation chips + final status pill; orb → `answer`.

---

## 6. Error handling

- Loop is best-effort + panic-free (today's `handle_voice_action` contract): any tool/brain/parse error → graceful `VoiceActionResult` status (`error`/`needs_consent`/`unavailable`), never a panic, never disrupts recording (runs on the existing detached dispatch thread).
- `AppError`/`Result<T>` everywhere; a locked-content refusal is `AppError::Locked`; a no-consent cloud turn surfaces as `needs_consent` (the floor still returns gated citations).
- Bounded: hard `max_steps` cap; `RESULT_BUDGET` truncation of re-fed tool output to bound cloud egress amplification + context growth.
- No PII in logs: log tool *names* + step counts + status only (matches `live.rs:246/322`); never args/results/answer/command text.

---

## 7. Security & privacy (binding — `lock-security-reviewer` gate)

- **Gate every read.** Every tool read routes through `GatedToolExecutor` → `execute_tool(…, unlocked, …)` → `visibility_clause`. The model supplies only strings; `unlocked` is host-held and grown only by biometric unlock commands (not in the tool set). A sealed-not-unlocked meeting surfaces NOTHING, including via the loop. **RED-before-GREEN test required.**
- **Redaction preserved by construction.** Cloud turns re-route through `make_provider`→`RedactingProvider`, so every egressing turn (incl. re-fed tool results in the prompt) is redacted. **No new egress class** vs today's single grounding call.
- **Route A vetoed.** Never point a cloud CLI at our MCP server in a Murmur-initiated loop (uploads raw gated content un-redacted; unseals the hermetic `--disallowedTools`). The local MCP server remains read-only for the *user's own* external clients only.
- **Writes (D2).** v1 loop is read-only. When write tools are later exposed to the model, they must be: additive/non-destructive by construction (no delete/blank/lock/unlock/export/overwrite), `meeting_is_visible`-gated, and **model-proposed writes require explicit user confirmation** (prompt-injection from third-party transcript speech is the threat — OWASP LLM01/LLM06). User-dictated writes stay auto-OK on the existing path.
- **Consent in the loop.** Unavailable connectors are **omitted from `specs()`** (least-privilege; the model can't call what it can't see), with the fail-closed sentinel as the second line; `specs()` is rebuilt per turn so a mid-loop revocation removes the tool immediately.

---

## 8. Testing (Definition of Done)

- **Static:** `cargo test --lib` + `npx ng lint` + `npx ng build` green (full `scripts/ci.sh` once at the end).
- **Headless loop tests** (with `StubReasoner` + a `MockReasoner` returning canned tool-then-answer, over the seeded visible/sealed fixture): sealed content never surfaces (RED-first); citations extracted from gated output; `max_steps` bounded; floor on non-convergence; panic-free on tool error; no `ToolCall` variant can mutate `unlocked` / do a destructive write.
- **FE smoke:** Playwright against `:1420` with mocked `__TAURI_INTERNALS__.invoke` + a mocked `Channel` — text composer prepends a pending row, trace chips render, answer resolves; no NG0600, no import-cycle.
- **Real-Mac-only (honest bar — not provable headless):** local multi-step tool-call reliability per GGUF, Polish quality, live latency feel, signed-build behavior. Captured via the local-reliability spike.

---

## 9. Phased plan (sequenced for `writing-plans`)

- **P0 — headless seam, zero behavior change.** `ToolSpec`/`ToolExecutor`/`GatedToolExecutor`/`AgentOutcome`/`agent.rs`/default `agentic()`; lift `tool_specs()`; implement `NoteAside`/`CreateReminder` in `execute_tool` (out of MCP allowlist + out of v1 autonomous set). Headless tests incl. the RED-first sealed-content + no-mutating-tool tests. Lock-security review. Nothing wired into dispatch.
- **P0-spike (real Mac) — local reliability.** Run `run_agentic_loop` over the registry GGUFs on 5–10 PL+EN commands; measure clean `{tool|answer}` rate + wall-clock. Outcome sets local `max_steps` (likely 1) and whether a grammar-router is needed.
- **P1 — dual-path voice (opt-in flag `agentic_voice`, default off).** `run_assistant_turn` + flag branch in the voice dispatch; map `AgentOutcome`→`VoiceActionResult`; FE unchanged. Adversarial verify + lock-security review.
- **P2 — text input + trace streaming.** `ask_assistant_text` command + `IpcService` method + the assistant-card composer; the `AssistantDelta` Channel (trace-only, D4); `AssistantStore`/`AssistantActionsComponent` trace chips + optimistic pending row.
- **P3 — retire hardcode to floor + token streaming.** Default `agentic_voice` on **ONLY after C1 (floor = deterministic path) is shipped AND the real-Mac spike passes a measured latency + answer-quality budget (C5)** — `handle_voice_action`/`rag_answer` are demoted to the floor, never deleted (they stay live on every stub/no-consent/non-convergent turn + for user-dictated writes). Add `Token` streaming with the redaction hold-back buffer (RED-first test that a split `⟪NAME_1⟫` is never observed partial and the restored output is byte-identical to non-streamed). Note: per **C8**, trace-only streaming alone leaves the synthesis frozen — pull `claude -p --output-format stream-json` synthesis streaming forward into P2 if the latency spike demands it.

Each phase is independently shippable, gated by `cargo test --lib` + adversarial-verifier (+ lock-security-reviewer for P0/P1/P3).

---

## 10. Open questions (carried from research)

- Local multi-step reliability + live latency on a real Mac (P0-spike decides local `max_steps` / grammar-router).
- Cloud egress amplification tuning (`max_steps`, `RESULT_BUDGET`) + `NoopNameRedactor` default disclosure.
- Exact `AssistantDelta` Channel ergonomics through the wake (no-invoke) path — confirm one channel registered at `init()` serves it.
- Whether to fold Flow A (note grounding) onto the same loop later (out of scope here).
