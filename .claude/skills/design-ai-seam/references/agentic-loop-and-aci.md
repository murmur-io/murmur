# Reference — the agentic loop envelope + the gated tool ACI

Deep material for `/design-ai-seam` step 4. When a task needs "the model decides", REUSE this envelope
and this ACI — do not roll a new loop or a new ungated tool path. All symbols verified against the
current tree (`agent.rs`, `tools.rs`, `orchestrate.rs`, `voice_action.rs`, `commands/{mod,ask}.rs`,
`transcribe/live.rs`).

---

## A. The loop envelope — `agent.rs::run_agentic_loop`

```rust
pub fn run_agentic_loop(
    reasoner: &dyn LocalReasoner,   // Cloud / Mistral GGUF / Stub — transport-agnostic
    system: &str,
    user: &str,
    executor: &dyn ToolExecutor,    // the GATED surface (specs() allowlist + run())
    max_steps: usize,
    sink: Option<&dyn DeltaSink>,   // live tool-trace chips (names + counts, no PII)
    opts: GenOptions,               // per-step token cap / timeout / compaction flag
) -> Result<Option<AgentOutcome>>
```

It drives the reasoner's `structured_with` primitive (schema-in-prompt + recover-JSON) in a bounded
decide-or-finish loop. Each turn the model replies with ONLY `{"tool":…,"args":…}` or
`{"answer":…}`.

### The bounds (the whole reason to reuse it, not re-roll)

- **`max_steps`** — hard turn cap.
- **Per-turn no-repeat dedup** — a `(tool,args)` pair already run is converted into an "already
  retrieved" marker instead of burning the budget (test `loop_no_repeat_guard_skips_duplicate_calls`).
- **`RESULT_BUDGET` (4000)** — per re-fed tool result, UTF-8-safe `truncate`; caps context growth AND
  cloud egress amplification step-over-step.
- **`TRANSCRIPT_BUDGET` (32_000)** + deterministic `LoopTranscript::compact` — once the whole loop
  transcript is over budget, OLD result blocks fold into a `[N earlier results omitted]` marker while
  the head (the user request) and the last `KEEP_LAST_BLOCKS` (2) blocks stay verbatim. **Marker
  blocks survive compaction** (the `[… failed …]` / `[… already retrieved …]` steering notes) even
  when they sit between evicted results, so the model never re-runs a failed/duplicate tool because its
  warning was compacted away.
- **One corrective retry on malformed JSON** — a `parse_first_json` error class (detected by
  `is_malformed_json_error`) gets exactly ONE retry with an explicit "reply with exactly one JSON
  object" instruction; a second failure propagates; every OTHER error class propagates immediately.

### The floor contract (load-bearing — do NOT put a floor inside the loop)

`run_agentic_loop` has **NO internal `reason()` floor**. It returns:
- `Ok(Some(outcome))` — the model CONVERGED to a non-empty answer;
- `Ok(None)` — did NOT converge within `max_steps` → the **CALLER** floors to the deterministic path
  (`handle_voice_action` / `rag_answer` / the deterministic context);
- `Err(e)` — propagated from `structured_with` (esp. `AppError::Unavailable` on no-consent) → NEVER
  swallowed, so the caller can floor AND emit `needs_consent` + gated citations.

Designing a new agentic caller means: build a `GatedToolExecutor` with the right scope, call
`run_agentic_loop`, and OWN the floor for `Ok(None)`/`Err`. Live callers to mirror: `voice_action.rs`,
`commands/{mod,ask}.rs` (the ask path), `transcribe/live.rs` (the cascade). `AgentOutcome` carries `answer`,
the ephemeral `steps` trace (tool NAME + ok only — never args/results; not persisted), and `citations`
(from `voice_action::extract_citations` over gated tool output only).

### The cascade escalation sentinel

`ESCALATE_SENTINEL` / `is_escalation(answer)` — a tier's prompt tells the model to reply EXACTLY
`{"answer":"__ESCALATE__"}` when the question isn't answerable at that tier; `is_escalation` fires ONLY
on the whole trimmed answer (a substring match would let a real answer that merely mentions the token
escalate). Distinct from `Ok(None)` (ran out of steps WITHIN the tier). This is how the brain cascade
climbs tiers in `transcribe/live.rs`.

### No new egress class

Every cloud turn re-routes through `make_provider` → consent gate + `RedactingProvider`. The loop adds
NO egress class of its own; redaction stays automatic. The on-device brain (mistral.rs GGUF) makes no
network call at all. So an agentic design NEVER needs a new firewall — it inherits the provider seam's.

---

## B. The gated tool ACI — `tools.rs`

The model can only NAME a tool + pass string args. It never reaches the DB. Reachability and execution
are decided by CODE, not prompt-trust.

### `AssistantScope` — the STRUCTURAL tier gate

```rust
pub enum AssistantScope { CurrentMeeting, Vault, Connectors, Full }
```

`AssistantScope::allows(tool)` is the tier gate applied ON TOP of the per-surface flags in
`GatedToolExecutor::specs`:
- **`CurrentMeeting` (Tier 1)** — NO retrieval tools at all (no vault reads, no connectors). Tier 1
  answers from PROMPT-INJECTED current-meeting content only; it must NOT be able to reach the vault.
- **`Vault` (Tier 2)** — the 6 owned-vault read tools (`search_meetings`, `search_semantic`,
  `get_meeting`, `list_recent_meetings`, `get_open_commitments`, `get_entity_dossier`); NO connectors.
- **`Connectors` (Tier 3)** — the connector/web tools (`web_search`, `calendar_lookup`, `jira_search`,
  `slack_search`) PLUS vault reads for grounding. Dynamic MCP tools (`mcp_<server_id>_query`) are
  connector-class — matched by prefix so a lower tier can never advertise or run one.
- **`Full`** — the full per-surface catalog (deliberately vault-wide surfaces: the Ask page, MCP-shaped
  reads).

**The design point:** tier isolation is enforced structurally — a weak model that mis-judges its scope
STILL cannot reach a higher tier's tools, because `specs()` filters the catalog by `scope.allows` and
`run()` re-checks the allowlist. There is literally no allowlisted path to call them. Tests:
`jit_scope_*` in `tools.rs` (e.g. `get_meeting` reachable at Vault/Connectors/Full but NEVER at
CurrentMeeting; `web_search` only at Connectors/Full).

### The read/write split

- **Reads** route through `execute_tool` (`fn execute_tool(...)`) — EGRESS-FREE (only the local SQLite
  DB + the local embedder), and NON-OPTIONALLY visibility-gated: `unlocked: &HashSet<String>` is a
  required arg, and every branch gates via `search_visible` / `search_hybrid_visible` /
  `meeting_is_visible` / `get_note_if_visible` / `list_meetings_visible` / `build_dossier_data`. There
  is no constructor that skips the gate.
- **Writes** live on `GatedToolExecutor`, NOT on the `execute_tool` `ToolCall` enum — so no read-only
  surface (e.g. MCP) can ever dispatch a write. `save_note` runs only when the executor was built with
  `allow_writes` AND re-checks `meeting_is_visible` against the live `unlocked` set before appending.
  `propose_note` (on `note_drafts` surfaces) writes NO DB — it records a draft in interior-mutable
  scratch for the FE to offer "Add to notes"; it needs no gate (touches no content store).
- **Connector tools** (`WebSearch`/`CalendarLookup`/`JiraSearch`/`SlackSearch`) are NOT runnable through
  the synchronous `execute_tool` — they dispatch through the async connector executors and only when
  the connector is exposed. This is why the ONE `ToolCall` that can egress (`WebSearch`) can't leak
  through the MCP surface's `execute_tool` path.

**Design rule:** a new tool is a `ToolSpec` in `tool_specs()` + an arm in `execute_tool` (read) or a
gated `GatedToolExecutor` write arm, placed in the right `AssistantScope` tier. A new tool that reaches
content MUST gate on the `unlocked` set; a new tool that egresses MUST go through a connector, never a
raw call.

### `ToolExecutor` / `DeltaSink` traits

`run_agentic_loop` talks to `trait ToolExecutor { specs(); run() }` and `trait DeltaSink`. The prod
executor is `GatedToolExecutor` (fields: `scope`, `allow_writes`, `note_drafts`, the db/unlocked/config
handles). A new caller wires a `GatedToolExecutor` at the right scope; a new EXECUTOR (rare) implements
`ToolExecutor` but must still gate every read.

---

## C. When a WORKFLOW beats an agent (the default answer)

`orchestrate.rs::orchestrate_context` is the exemplar of choosing the RIGHT rung: it uses the brain to
DECIDE a retrieval PLAN, but each planned query maps to a gated `ToolCall` run through `execute_tool` —
and when the reasoner is the dependency-free `StubReasoner` (`id() == "stub"`, the default no-model
build) it falls THROUGH to the deterministic `pipeline::build_grounding_context`, byte-identical. Any
reasoner error / plan-parse failure / empty corpus ALSO degrades to that deterministic floor. It NEVER
fails the pipeline (`Option<String>`, never `Err`).

The ladder for a new capability — climb only when the lower rung can't:

1. **Workflow** (deterministic, zero model) — fixed steps + gated reads + template synthesis. The
   floor `orchestrate_context` falls back to; `verify.rs` (deterministic claim-checking) is a pure
   workflow.
2. **Tooled call** — one gated `execute_tool` / `ConnectorRegistry::search` behind a deterministic
   caller. No loop.
3. **Router** — a `route(RouterInput) -> RouteDecision` fork over roles/postures/availability (pure,
   testable; see `seam-cutover-and-eval.md`).
4. **Orchestrator** — the model plans, CODE executes the plan through gated tools (the
   `orchestrate_context` shape). Bounded, no free-running loop.
5. **Agent** — the free-running `run_agentic_loop` (the model decides tool-by-tool until it answers).
   The MOST power and the most bounds to respect; use only when the task genuinely needs
   decide-tool-by-tool iteration.

**Default answer to "add an agent": can a workflow or a single tooled call do it?** If yes, spec that —
an agent a workflow could do is over-engineering and a bigger surface to gate. When you DO reach for the
agent, you inherit every bound in §A for free — that is the payoff for reusing the envelope.
