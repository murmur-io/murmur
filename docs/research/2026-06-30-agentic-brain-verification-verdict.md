<!-- Generated 2026-06-30 via the verify-agentic-brain Workflow (16 agents: 6 adversarial critiques × independent verify + 3 prior-art angles + synthesis). Verdict confidence: HIGH. -->
# Verification verdict — Agentic-brain design (D1–D6) + prior-art contrast

**Method:** 6 independent adversarial critics (one per design decision) each tried to BREAK the decision; each strongest attack was then independently verified as real-vs-hallucinated against the actual tree; 3 prior-art angles (our brain2 docs / external SOTA / competitor brains) contrasted in parallel; one synthesis. See the design under test: `docs/superpowers/specs/2026-06-30-agentic-assistant-design.md` + `docs/superpowers/plans/2026-06-30-agentic-assistant-p0.md`.

## Verdict: BUILD IT — the architecture is right — but NOT as specified. 6 required fixes.

`design_survives: true`. The core call (replace the hardcoded router with a bounded, read-only, flag-gated tool-use loop on the existing `structured()` primitive) is sound and verified-correct on the two load-bearing axes: **D1** (structured over native — native tool_use is unreachable on the default `claude_code` CLI provider without the vetoed redaction-bypass; structured() keeps the RedactingProvider firewall automatic on every egressing turn) and **D4** (gating + redaction airtight; no sealed leak found; Route-A veto correct). **No fundamentally better architecture emerged** (`better_brain_architecture: "none"`). The local-first + agentic + gated-visible-sources combination is a genuinely **unoccupied competitive quadrant**.

But two attacks were verified REAL and BREAKING-as-specified (D2, D6), two WEAKEN (D3, D5), and there is one shared root defect.

## Per-dimension result

| Dim | Decision | Verdict | What's real |
|---|---|---|---|
| **D1** | structured() loop vs native tool-use | **holds** | Right call. *But* single-tool-per-turn at `max_steps=1` is narrower than the multi-read fan-out → emit a bounded ARRAY of tool calls per turn (optional). |
| **D2** | read-only v1, writes deferred | **BREAKS** | Whole-dispatch flag + read-only loop + P3-default-on = voice-dictated `CreateReminder`/`NoteAside` (live today) **silently stop working**, answered as questions. Project's #1 failure mode; zero P0 test. |
| **D3** | local bounded 1-round + floor | **weakens** | The P0 floor is `reason()` over gathered, **not** the deterministic fan-out → "zero regression either way" is FALSE. At `max_steps=1` local loses the literal-Polish cross-lingual leg. |
| **D4** | gating + redaction + Route-A veto | **holds** | No sealed leak. 2 residuals: re-snapshot `unlocked` per turn (mid-loop relock); web_search args egress with regex-only redaction (NoopNameRedactor leaves names). |
| **D5** | latency / "live" feel | **weakens** | Default `claude_code` = fresh `claude -p` subprocess per turn, **measured ~3–6s**, no streaming → loop **doubles** today's single pass. "3–8s w/ caching+streaming" is from a path that isn't built. Cluely-300ms framing wrong. |
| **D6** | migration / floor / trace | **weakens** | Same floor defect: on default (Cloud + consent-off) turn-0 `structured()`→`Err(Unavailable)`, `Err(_)=>break` swallows it → empty answer, no citations, no `needs_consent`. AgentStep trace never durable in any phase. |

## The single root defect (under D2+D6+D3)

**The P0 loop's FLOOR is `reasoner.reason(SYNTH, gathered)` over whatever the loop accumulated — NOT the deterministic fan-out (`rag_answer`/`handle_voice_action`) that the spec line 20 *calls* the floor.** On the **shipped default** (`BrainBackend::Cloud` + `cloud_egress_consented:false` + `provider claude_code`, all verified `config.rs:24/196/222`):
1. turn-0 `structured()` → `Err(AppError::Unavailable)` (proven `reason.rs:867-878`),
2. the loop's `Err(_) => break` swallows it (plan ~520), `gathered` is empty,
3. floor synth is empty, `extract_citations("")==[]`,
4. → `AgentOutcome { answer:"", steps:[], citations:[] }`.

Today's `rag_answer` runs the gated fan-out **before** the brain and returns `needs_consent` + gated `[[Title]]` citations on no-consent (`voice_action.rs:491-500`). So flipping `agentic_voice` on (P3) regresses the **most common user** from "needs_consent + cited related meetings" to an empty, citation-less answer — and the read-only loop + reason()-only floor reach **neither** write arm, so dictated reminders/notes silently die. **Biggest risk:** the project's signature failure mode (green build/lint, capability gone at runtime, no test catches it) on nearly every user.

## Required changes (only from attacks verified REAL)

1. **FLOOR = the deterministic path, not `reason()`-over-gathered.** On non-convergence / `structured()` Err / stub, fall through to `resolve_command_intent` → `handle_voice_action`/`rag_answer` (gated `SearchMeetings`+`SearchSemantic`+dossier+literal-PL retrieval + cited synthesis + `needs_consent`). Makes "demote, never delete" actually true and `rag_answer` load-bearing, not dead code behind an always-on flag.
2. **DECOUPLE writes from the whole-dispatch flag.** At the entry, route an explicitly **user-dictated** `CreateReminder`/`NoteAside` through `handle_voice_action` (a legitimate write-vs-read routing decision — categorically NOT "the model picks a read tool", so it doesn't betray zero-hardcoded-routing for the part that matters); route only **informational** turns to the loop. (Folds into #1 if `handle_voice_action` is the floor.)
3. **PROPAGATE `AppError::Unavailable`** out of the loop instead of `Err(_) => break`, so the mapper emits `needs_consent` + the gated citations the floor gathered.
4. **ADD RED-first regression tests in P0** (all headless-doable now): (a) `structured()` returns `Err(Unavailable)` → yields gated citations + `needs_consent` (RED against the break+empty-synth); (b) stub-floor ≡ `rag_answer` equivalence (mirror `orchestrate.rs:389`); (c) "the agentic path still executes a user-dictated reminder/aside".
5. **DO NOT default `agentic_voice` ON in P3** until the floor is fixed AND the real-Mac spike passes a **measured budget** scoring answer QUALITY vs the fan-out on ~15–20 PL+EN commands (per our own prove-before-swap prior, `DESIGN-local-brain-orchestration.md:9,54`). Until proven, **LOCAL voice defaults to the deterministic fan-out**; stop overselling "local agentic live".
6. **RE-SNAPSHOT `unlocked` per turn** (not once at loop start) — a mid-loop screen-share auto-relock leaves the snapshot stale across a 10–25s loop. Rebuild it each turn alongside the `specs()` rebuild; add a RED-first "folder relocked mid-loop → subsequent tool calls surface nothing".

## Optional improvements (from prior-art lessons)

- **Bounded ARRAY of tool calls per turn** (the shipped `orchestrate.rs Vec<RetrievalQuery>` shape) + `join()` them → match the fan-out's breadth; parallelize the floor's currently-serial fan-out (`voice_action.rs:302`).
- **Pull streaming-synthesis into v1** (`claude -p --output-format stream-json` through the redaction hold-back) — the token-by-token final pass is THE live-feel lever; trace-only leaves the worst half frozen. Reframe positioning to "fast grounded cited assistant" (~p50<10s / p95<18s cloud), not a sub-second overlay.
- **Tool-use examples in the catalog prompt** (Anthropic measured 72%→90% on complex params) — cheapest local-Q4 reliability lever before grammar constraints; the tiny `Constraint::Regex` tool-name router (dodges the Bielik 32K llguidance overflow) is the documented fallback.
- **No-repeat / loop-progress guard** (ReAct's documented non-termination failure): dedup a tool+args pair already run this turn.
- **Make the AgentStep trace durable OR delete its "flywheel" contract** — it's durable in NONE of the 4 phases as drawn. If persisted, **lock-review it**: the trace carries args + gathered sealed-derived grounding and MUST be `meeting_id`-scoped, visibility-gated, and purged-on-seal exactly like `correction_log` was fixed (the prior 🔴 flywheel lock bug, `DESIGN:26-38`).
- **Gate `search_semantic`'s ToolSpec on model-presence** — on a fresh install (`semantic_search_enabled` default false, `StubEmbedder`) it returns hash-bag noise; advertising it unconditionally lets the model ground in non-semantic vectors.
- **Adopt Granola's double-click-to-verify citations** — make each `[[Title]]` chip a deep-link into the gated source (`obsidian://` block-ref we already emit). Turns the visibility guarantee into a visible trust feature.
- **Fold Flow A (note grounding) onto the same loop as an explicit P4** — running two retrieval paradigms indefinitely violates "one brain, one registry".

## Prior-art contrast (summary)

- **Our brain2 docs → PAR.** Consistent evolution (orchestrate.rs Flow A IS the `max_steps=1` ancestor; "connectors = live tools the brain calls" generalized). BUT it re-opens the twice-stated **"agentic-RAG-overkill trap"** (`DESIGN:9`, `intelligent-rag:48`) → the design's own guardrails (`max_steps=1`, deterministic floor, structured-not-native, read-only) must be treated as **BINDING**, and the real-Mac quality spike a **HARD GATE before P3**. Reconcile the native-tool-calling reversal explicitly (we drop the grammar-constrained reliability lever the 2026-06-28 decision chose, due to the Bielik 32K overflow).
- **External SOTA → PAR-to-AHEAD.** Loop shape is textbook (matches Anthropic "Building Effective Agents" + Agent SDK `max_turns`, ReAct, LangChain "generate" early-stop). Every shipped-pitfall guard is independently validated by a primary source (tool-hallucination paper → `is_advertised()`; context-rot → `RESULT_BUDGET`; bloated-tool-sets → tight scoped catalog). **AHEAD**: building on `structured()` makes redaction automatic per egressing turn — a property native tool_use does NOT give for free. **Trail by choice**: native tool_use primitives (parallel/strict-schema/prompt-caching) + persistent self-editing memory (mem0/Letta) — correctly deferred.
- **Competitor brains → AHEAD in an empty quadrant.** Cloud players (Granola/Otter/Tana) are agentic but cloud + send transcripts to third-party LLMs; local players (Meetily/Natively) are single-shot, not agentic. **Nobody ships a local-first agentic gated-source meeting brain** — our quadrant. Behind on latency/polish, write-action maturity, longitudinal memory (don't chase the last — it's a cloud/multi-tenant play off our strategy). Cluely's mid-2025 83k-user transcript breach = concrete positioning collateral for "this cannot happen here, by construction".

## Sources
Full structured output (6 critiques + verifications, 3 prior-art briefs, synthesis): the workflow run `wf_1c28e939-350`. Key external refs: Anthropic Building-Effective-Agents / Agent-SDK / advanced-tool-use / context-engineering; ReAct (2210.03629); Reflexion (2303.11366); Tool-Hallucination "Reasoning Trap" (2510.22977); Agentic-RAG survey (2501.09136); mem0/Letta/Cognee landscape; Granola/Otter/Tana/Meetily/Natively/Cluely. Key code: `config.rs:24/196/222`, `reason.rs:867-878`, `voice_action.rs:288-320/475/491-500/533`, `commands.rs:1154`, `orchestrate.rs:112-160/123/389`, `tools.rs:9-14/69`, `reason/mistral.rs:133-146`.
