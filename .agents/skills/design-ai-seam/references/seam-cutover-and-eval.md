# Reference — the simplest-pattern ladder, seam-when-earned, shadow cutover, eval gate, handoff spec

Deep material for `/design-ai-seam` steps 1, 6, 7. All symbols verified against the current tree
(`router.rs`, `transcribe/live.rs`, `scripts/ci.sh`, `eval/`).

---

## A. The simplest-pattern-first ladder (YAGNI, restated)

`workflow  <  tooled call  <  router  <  orchestrator  <  agent`
(full definitions + the exemplars for each rung: `agentic-loop-and-aci.md` §C.)

Climb ONE rung only when the lower one genuinely can't serve the requirement. The DEFAULT answer to
"add an agent" is "can a deterministic workflow or a single gated tooled call do it?" — an agent a
workflow could do is over-engineering and a larger surface to gate. Justify the rung you land on in the
spec; if you land on "agent", state which lower rung you rejected and why.

---

## B. Seam-when-earned (add an abstraction only for a SECOND consumer)

Do NOT introduce a trait/enum/abstraction "to be flexible." Add it when a SECOND real consumer exists.
Two shipped precedents:

- **`tools.rs` is ONE `execute_tool`** because the doc says it plainly: "Today the MCP server is the
  only caller; tomorrow the local brain dispatches the same `ToolCall`s. Keeping ONE `execute_tool`
  means exactly one gated implementation of each tool — a future surface cannot accidentally grow an
  ungated path." The seam earned itself: a second consumer (the brain) was concrete, and the value is a
  single gate, not speculative flexibility.
- **`supports_native_json()` is a capability seam with NO dispatcher yet** (`provider-and-egress-seam.md`
  §A). That is the ONE case where a seam ahead of its consumer is fine: it changes NO behavior (a
  default that keeps every provider byte-identical), so it costs nothing until the cutover is justified
  by data. If a proposed seam WOULD change behavior before its second consumer exists, defer it.

---

## C. Shadow-log parity cutover — `router.rs` (never big-bang)

`router.rs` is the reference implementation of cutting a new decision layer over the legacy path
SAFELY. Read the module doc: it is "ADDITIVE, not yet wired into dispatch. Per spec §L3, NOTHING
dispatches through this yet — the legacy paths keep deciding."

The pattern:

1. **Build the new decision as a PURE, exhaustively-testable function.** `route(RouterInput) ->
   RouteDecision` (`DeterministicFloor` / `LocalLight` / `LocalHeavy` / `CloudAgentic{connection}`),
   keyed on the role's RESOLVED connection (`roles::resolve`). `RouterInput` injects the
   caller-probed booleans (`heavy_available` / `light_available` via `class_model_available`) so the
   decision table stays pure — no I/O in `route`, no consent/egress logic (those stay in the provider
   factory and can NEVER be bypassed by a routing decision). `classify_query -> QueryClass` is
   likewise pure (keyword classifier; a model classifier can slot behind the same enum later).
2. **Wire ONLY a shadow log next to the live path.** In `transcribe/live.rs` the sole integration point
   computes `router::route(&RouterInput{…})` and logs it CONTENT-FREE (`shadow = decision.label()`,
   never the query) NEXT TO what the legacy path actually chose. `RouteDecision::label()` /
   `QueryClass::as_str()` are stable, PII-free log tokens for exactly this.
3. **Validate parity on REAL usage.** Divergence is EXPECTED and MEASURED (e.g. the router plans local
   tiers the legacy path floors on) — the shadow log is what quantifies it before any flip.
4. **THEN flip**, once the shadow data justifies it. Never big-bang a new default over the legacy path.

Adversarial lessons already baked into `router.rs` (cite them so a redesign doesn't re-pay them):
External keywords match on WORD BOUNDARIES (`contains_word`) — Polish inflections routinely CONTAIN
them ("webinaru" ⊃ "web", "newsletterze" ⊃ "news") and a substring match POISONS the shadow-parity
data; "online" was dropped as too ambiguous (owned content as often as the web).

**Design rule:** any new routing/dispatch decision follows this — pure `route`-style function →
shadow-log parity → flip. Put consent/egress NOWHERE in the router; the target is named, the gates live
in the factory.

---

## D. The eval gate — measure the delta, don't feel it

A prompt / model / tool / retrieval change reports its eval-harness delta. The harness is in `eval/`
(`eval/bakeoff.rs`, `eval/mod.rs`, `eval/diarization.rs`).

- It is **MANUAL, not in CI.** `scripts/ci.sh` says so explicitly: "the RAG eval gate (`eval::bakeoff`
  `#[ignore]` runners + `eval/results/` artifact) is MANUAL — it needs the embed model (and, for the
  real-vault run, a copied DB) so it is NOT run in CI; see `docs/RAG-BAKEOFF.md` for the re-run command
  + merge rule."
- So the runners are `#[ignore]`d (they need a downloaded embedder / a real DB) and produce an
  `eval/results/` artifact. A retrieval/reranker/fusion change reports the metric delta (recall@k /
  nDCG per the bakeoff) as EVIDENCE, and merges per the `docs/RAG-BAKEOFF.md` rule.

**Design rule:** if the seam touches retrieval quality or prompt/model behavior, the spec names the
eval metric it should move and how it's measured (which bakeoff runner, what the baseline is). "It feels
better" is not a delta.

### The real gate for everything else

`scripts/ci.sh` is the one-shot final gate (hooks selftest → swiftc → clippy `-D warnings` → `cargo
test` → cargo audit/deny → cargo build → ng lint → ng build → E2E). In the ITERATE loop use
`cargo test --lib` from `src-tauri/`; NEVER `cargo clippy --all-targets` in the loop (it thrashes the
openssl/sqlcipher profile and times out). A new EGRESSING `SummarizeRequest` field ALSO extends the
redaction coverage-guard (`provider-and-egress-seam.md` §C) — that is part of "green", not optional.

---

## E. The handoff spec skeleton (what `/design-ai-seam` emits)

The deliverable is a decision-ready spec that an orchestrator can dispatch. Skeleton:

```
DECISION: <seam location + rung on the ladder, one line>
LANE: architecture (HOW it fits) — not /research (whether) / not /ship-feature (mechanical build)

SEAM:
  file:symbol that changes  — e.g. summarize/provider.rs::SummarizerProvider (add method X, default Y)
  rung + why not higher     — workflow / tooled / router / orchestrator / agent
  trait/enum/method shape    + its SAFE DEFAULT (keeps existing impls byte-identical)

GATES & INVARIANTS (map each to the numbered invariant):
  egress   — covered by make_provider_resolved / ConnectorRegistry::search (never a call site)
  redaction+ledger — placement; new egressing field ⇒ extend the coverage-guard test
  reads    — the visibility gate for each new read (meeting_is_unlocked / visibility_clause / *_visible)
  loop     — max_steps + AssistantScope tier (if any tool)
  store    — SQLite canonical (new surface = thin reader)
  verify   — deterministic / code-owned (never LLM-judge)

SEAM-WHEN-EARNED:
  second consumer today? yes → wire it | no → shadow-log parity plan (router.rs pattern), defer cutover

EVAL & COVERAGE:
  eval metric moved + how measured (bakeoff runner / eval/results); coverage-guard extension; ci.sh

DISPATCH (file-disjoint):
  rust-tauri-dev      → <backend seams: the trait/factory/tool/gate work>
  angular-zoneless-dev → <FE: one ipc.service.ts method per new command + DTO in core/models.ts + signals>
VERIFY:
  adversarial-verifier    (ALWAYS)
  lock-security-reviewer  (REQUIRED iff reads/exports/crypto/keychain/MCP/egress touched)

SIGNED-BUILD-ONLY (honest boundary):
  Touch ID / lock-at-rest / real connector egress / packaged WKWebView / the shadow-parity window
```

### Dispatch discipline

- **Split by file-disjoint layer.** Rust backend seams → `rust-tauri-dev`; zoneless FE (IPC method +
  signals + Liquid-Glass view) → `angular-zoneless-dev`. Backend and FE are usually disjoint files →
  they can run in parallel, then serialize anything that shares a file (per `.codex/rules/agentic-workflow.md`).
- **The implementer never owns the verdict.** Route to `adversarial-verifier`; add
  `lock-security-reviewer` as a REQUIRED second gate whenever the seam touches
  reads/exports/crypto/keychain/MCP/egress. The architect self-checks but does NOT self-certify.
- **Inject prior lessons.** Before dispatching a role, prepend its curated `## Recurring patterns` from
  `.codex/learnings/<agent>.md` as "Previous lessons (binding — do NOT repeat these)" — the
  compounding-lessons loop.
