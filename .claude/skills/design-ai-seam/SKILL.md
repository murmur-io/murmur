---
name: design-ai-seam
description: The seam-decision checklist for Murmur — how a new AI capability, model, provider, tool, connector, or surface should fit the existing seams (the SummarizerProvider trait + capability methods, the ONE egress-consent-redaction-ledger seam, the agentic-loop envelope, the gated tool ACI, routing shadow-cutover, SQLite-canonical readers, the eval gate). Use proactively when a task is "add/change an AI capability and the question is HOW it fits" — it sits between /research (whether to build) and /ship-feature (the mechanical build). Outputs a decision-ready spec naming the seam location, the trait/enum shape, the gate placement, the loop bounds, the eval-delta plan, and which implementer(s) + verifier(s) to dispatch.
---

# /design-ai-seam — decide WHERE an AI seam goes in Murmur

You are placing a new AI capability (a model, provider, tool, connector, surface, or agentic behavior)
into Murmur's **existing** seams — without a leak, a fork, a needless agent, or a fourth diverging copy
of the truth. This skill is the checklist; the deep, code-grounded material lives in `references/`
(load a file when you reach its step). It pairs with the **`ai-systems-architect`** agent — dispatch
that agent (or follow this yourself) to produce the decision-ready spec, then hand the build to
implementers and the verdict to `adversarial-verifier`.

**Lane check first.** This is the INWARD "how does it fit" layer. If the real question is "should we /
can we build X" → `/research` (`murmur-researcher`, outward). If it's "build this agreed change
end-to-end" → `/ship-feature` (mechanical + verified). You are between them.

**Trust code, not docs.** Every symbol below is current, but grep it before you rely on it —
commands and storage are split across growing domain modules, and line numbers drift. Cite by symbol.

## 1. Simplest-pattern-first ladder (start here, every time)

`workflow  <  tooled call  <  router  <  orchestrator  <  agent`

The DEFAULT answer to "add an agent" is **"can a deterministic workflow or one gated tooled call do
it?"** Climb a rung only when the lower one genuinely can't. Deep decision table + when a workflow
beats an agent: `references/agentic-loop-and-aci.md` and `references/seam-cutover-and-eval.md`.

## 2. Provider changes ride the CAPABILITY seam

A provider/model difference is a `SummarizerProvider` capability method with a safe default — the
`supports_native_json()` pattern (`summarize/provider.rs`, default `false`; only the gateway
overrides). **Never** a lowest-common-denominator flatten that strips a capable provider, and **never**
a forked second trait/path. Deep pattern + the `complete_json_with_meta` default-delegation shape:
`references/provider-and-egress-seam.md`.

## 3. The ONE egress seam (non-bypassable by design)

Every cloud/network path routes through **`make_provider_resolved`** (`summarize/mod.rs`) or
**`ConnectorRegistry::search`** (`connectors/mod.rs`). Both put the fail-closed consent gate,
`egress_is_cloud` classification, the `RedactingProvider` firewall, and the content-free egress-ledger
row INSIDE the factory/registry, keyed on the resolved connection — so a call site can't forget them.
**No raw provider, no direct `reqwest` to a model/service, at a call site.** A new EGRESSING
`SummarizeRequest` field must extend the coverage-guard test
`every_string_field_of_summarize_request_is_scrubbed_or_exempt`. Full firewall map + `EgressClass` +
the coverage-guard war story: `references/provider-and-egress-seam.md`.

## 4. Agentic loop envelope + the gated tool ACI

Any agentic behavior REUSES `run_agentic_loop` (`agent.rs`): `max_steps` + per-turn no-repeat dedup +
`RESULT_BUDGET` + marker-preserving `LoopTranscript::compact` + one malformed-JSON retry. The loop has
**no internal floor** — `Ok(None)` = non-convergence (the CALLER floors), `Err` (esp. `Unavailable`)
propagates. Tools are a curated, gated ACI keyed by **`AssistantScope`** in CODE (`tools.rs` —
`allows()` filter + `run()` allowlist), never prompt-trust; reads go through the visibility-gated
`execute_tool`, writes through `GatedToolExecutor` with `allow_writes`. Deep envelope + tier table +
"workflow-beats-agent" test: `references/agentic-loop-and-aci.md`.

## 5. Store & reads (SQLite is canonical)

A new consumption surface (UI / MCP / export / anything) is a THIN reader over the one SQLite store,
gated by `meeting_is_unlocked` (commands) or `visibility_clause` / the `*_visible` helpers (db/MCP) —
never a fourth diverging copy. A new content read or export that isn't gated is a leak (fails the
`lock-security-reviewer` gate). See `.claude/rules/lock-model.md` (do not duplicate it — read it).

## 6. Seam-when-earned + shadow cutover (YAGNI)

Add a trait/enum/abstraction only when a **SECOND real consumer** exists. Cut a new default over by
**shadow-log parity** — the `router.rs` pattern (ADDITIVE, not yet wired: a content-free line in
`transcribe/live.rs` logs the would-be `route()` decision next to the legacy path's choice, validated
on real usage BEFORE any flip). Never big-bang. Deep cutover recipe: `references/seam-cutover-and-eval.md`.

## 7. Eval gate + the handoff spec

A prompt/model/tool/retrieval change reports its **eval-harness delta** (the `eval::bakeoff` `#[ignore]`
runners + `eval/results/` artifact — MANUAL, needs the embed model / a copied DB; `docs/RAG-BAKEOFF.md`)
and routes through `scripts/ci.sh` (`cargo test --lib` in the loop; NEVER `clippy --all-targets`
there). Then emit the **handoff spec**: the seam location, the trait/enum shape + safe default, gate
placement, loop bounds, eval-delta plan, and the dispatch — file-disjoint implementers
(`rust-tauri-dev` for backend seams, `angular-zoneless-dev` for the FE IPC+signals) and verifiers
(`adversarial-verifier` always; `lock-security-reviewer` REQUIRED iff the seam touches
reads/exports/crypto/keychain/MCP/egress). The handoff-spec skeleton:
`references/seam-cutover-and-eval.md`.

## Rules

- **Design only — read-only on app code.** This skill produces a spec and dispatches builders; it does
  not write production code and does not own the verdict (adversarial-verifier does). Self-check, never
  self-certify.
- **Prefer the lowest rung.** An agent a workflow could do is over-engineering — spec the workflow.
- **Never spec a bypass** of the one egress seam, a provider-trait fork, an ungated read, an
  LLM-as-judge, or an unbounded loop. Surface the tension and redesign.
- **Seam only when earned** (second consumer); cut over by shadow-parity, never big-bang.
- **The signed-build boundary is honest** — Touch ID / lock-at-rest / real connector egress / packaged
  WKWebView only truly verify on a Developer-ID build on a real Mac.
- No new npm packages or crates without explicit user approval. `com.meetnotes.app` immutable. No PII
  in logs.
