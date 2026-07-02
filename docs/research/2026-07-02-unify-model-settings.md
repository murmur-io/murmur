<!-- Generated 2026-07-02 via /research (murmur-researcher fan-out: code map, competitor UX patterns, per-role architecture, Settings IA audit). Pricing/version/competitor facts = point-in-time. -->
# Research: Unifying AI provider/model steering — Settings IA + per-use model architecture

## TL;DR / Verdict

The user's "rozwalone" diagnosis is code-confirmed. The AI surface is split across **4 settings sections + onboarding + a record-screen banner**, and the fragmentation is almost entirely an **IA/naming problem, not an engine problem**: the backend already funnels *every* LLM text call through one factory (`make_provider(&config.provider_id, &config)`, `summarize/mod.rs:78`) plus one dispatch layer (`brain_backend` → `ReasonerCell`, whose `Cloud` arm loops right back into `make_provider`). Nothing hardcodes a model; there are exactly two knobs — but the UI renders them as five controls in four places with three overloaded words ("provider" / "backend" / "model") and **four factually false help texts**.

**Target design (both pillars):**
1. **UX/UI:** one **"AI & Models" hub** replacing `Brain & AI` + `Providers` + General's provider dropdown — provider **connection cards** (Local vs Cloud split, Test button, gateway config inline and always visible) → a **"What Murmur uses" assignment block** (one Default row + per-feature overrides behind progressive disclosure) → a **privacy strip** (what stays on-device / what leaves redacted, consent state + revoke, activity link). This is the industry-converged pattern (Zed, BoltAI, Jan, Smart Composer); a full Continue.dev-style role matrix would be over-engineering.
2. **Architecture:** a small fixed set of model **roles** — `Default` + three user-visible overrides **Notes / Ask / Live** — resolved by one pure function `resolve(role, &AppConfig) -> RoleTarget` next to `make_provider`. Role keys are 9 additive settings rows; absent keys fall back to today's `provider_id`/`provider_model`/`brain_backend` **exactly**, so the backend PR is provably zero-behavior-change. `brain_backend` collapses into the Ask/Live dropdown ("Local model" and "Off" become selectable targets). Embeddings/NER/transcription stay out of the picker — they are local-only, model-presence-activated, and should be shown as fixed "always on-device" badges.

**Hard prerequisite:** split the 3,713-line `settings.component.ts` into per-section child components first — its styles measured **15.51 kB against the 16 kB error budget** in a real `ng build` (2026-07-02). The redesign cannot land in the monolith.

## What we already have (from the repo, verified file:line)

### The two engines (both already unified underneath)

- **Engine A — `make_provider(id, &AppConfig)`** (`summarize/mod.rs:78`): builds the one active provider from `provider_id`, applies the fail-closed `cloud_egress_consented` gate (`mod.rs:86-92`), wraps every cloud-classified path (incl. remote Ollama, `egress_is_cloud` `mod.rs:61-70`) in `RedactingProvider` (`mod.rs:188-197`), and records the egress ledger (`mod.rs:169-187`). Model per arm: `claude_code` → `provider_model` as `--model` (`mod.rs:99`); `anthropic` → `provider_model` **else** `anthropic_model` (`mod.rs:108-112`); `ollama` → `ollama_model`; `gateway` → `gateway_model` (`mod.rs:119-146`).
- **Engine B — `ReasonerCell` on `brain_backend`** (`reason.rs:259-329`): `Cloud` → `CloudReasoner::build_provider` re-reads config fresh and calls `make_provider(&cfg.provider_id, &cfg)` (`reason.rs:577-586`) — so "Claude (cloud)" actually means "whatever the General provider is"; `Local` → cached mistralrs GGUF (`brain_model_id` from the `BRAIN_MODELS` registry, `reason.rs:54-86`); `Off` → StubReasoner.
- **Not config-steered at all:** the e5 embedder (`embed.rs:146`) and the NER name redactor (`redact.rs:79`) activate on **model presence on disk**, local-only. Proactive hints are deterministic, zero-egress (`proactive.rs`). MCP is read-only, no provider (`mcp.rs:670`).

### Consumer → resolution map (nothing hardcodes a model)

| Consumer | Engine | Effective provider+model | Natural role |
|---|---|---|---|
| Note summary (`pipeline.rs:572`), auto-organize (`pipeline.rs:883`), graph extraction (`commands.rs:1775`), recipes (`:1490`), digest (`:2699`), dossier (`:2638`), brief (`:2854`), timeline (`:3587`), meeting chat (`:1247`) | A | `provider_id` + per-arm model | **Notes** |
| Flow-A pre-analysis (`pipeline.rs:550`), fact extraction (`commands.rs:1857`) | B | `brain_backend` | Notes (internal) |
| Ask agentic loop (`commands.rs:2022`; gated `brain_backend == Cloud` at `:1972`) | B(Cloud)→A | Cloud only; Local/Off never run it | **Ask** |
| Ask floor (`commands.rs:2189-2203`) — the ONLY Ask path on Local/Off | **A** | **`provider_id` — ignores `brain_backend` entirely** | Ask |
| In-meeting assistant / @brain threads / voice (`live.rs:368/405/465`; dispatch gate = `realtime_reactions` only, `live.rs:304`) | B | Cloud→agentic via A; Local→GGUF floor synthesis | **Live** |
| Embedder / NER / proactive / MCP | — | local, presence-activated / deterministic | fixed badges |

### The four semantic traps (each verified, high confidence)

1. **`brain_backend=Cloud` ≠ "Claude cloud"** — with `provider_id=ollama` the "Claude (cloud)" backend runs on local Ollama; with `gateway` it goes to the user's proxy. The help text "Sends your (redacted) text to Anthropic's cloud" (`settings.component.ts:605-608`) is wrong for both.
2. **The Brain & AI "Model" picker (`provider_model`) under- and over-claims at once** — copy says "Overrides the model used for grounded answers" (`:694-697`) but it steers **every** claude_code/anthropic call including note summaries; and it is **silently inert** for gateway/ollama (those arms read `gateway_model`/`ollama_model`), yet always rendered with 3 hardcoded Claude ids. Passing those ids through `claude_code --model` to a LiteLLM proxy is the exact v0.6.1 regression already paid for.
3. **"Local model — fully on-device" does NOT localize most AI** — on `Local`, note summaries, Ask (floor), chat, recipes, digest, dossier, brief, timeline, graph still egress via `provider_id`; only pre-analysis, facts, and the in-meeting floor go local — and the agentic loops get *disabled* (Cloud-only gates `commands.rs:1972`, `live.rs`).
4. **"Off" doesn't turn the in-meeting assistant off** — wake-word dispatch keys only on `realtime_reactions` (`live.rs:304`); it answers via the deterministic floor.

### The full inconsistency list (from the code-map + IA audits)

- **T1 Gateway split-brain:** pick "AI Gateway" in General → config card lives in Providers **and renders only when gateway is already selected** (`settings.component.ts:1108`).
- **T4 Duplicate model fields:** "Anthropic model" free-text (Providers, `anthropic_model`) vs "Model" dropdown (Brain & AI, `provider_model`) — `provider_model` silently wins (`mod.rs:108-112`); precedence expressed nowhere.
- **Dead control:** "Custom GGUF model" input binds `brainModelId` but `dto_to_config` silently discards non-registry values (`commands.rs:3126-3130`); the field that would accept a path (`brain_model_path`) is preserve-only, not FE-settable (`commands.rs:3120-3122`).
- **Consent surface divergence:** the FE realtime-consent banner fires only for `claude_code|anthropic` (`settings.component.ts:651-652`) but the backend classifies gateway + remote-ollama as cloud too (`mod.rs:61-70`) — a gateway user gets no warning, then a fail-closed runtime error. Consent is also grant-only (no revoke, `:3505-3510`) and duplicated in two sections.
- **Onboarding omits gateway** (`PROVIDER_LABELS`, `onboarding.component.ts:27-31`) and never asks consent → a fresh cloud user's **first note fails by design**, recovering via a record-screen banner with hardcoded "Anthropic's cloud" copy (`record.component.ts:238-239`).
- **Hardcoded Polish consent copy** in the English UI (`settings.component.ts:657-659`); Notes section says "how **Claude** writes each summary" regardless of provider (`:507`); Privacy still claims "names are **not** redacted — regex-only" (`:1414-1419`) despite the shipped NER layer (`mod.rs:159-164`) — accidentally true only because the NER download command (`lib.rs:163-164`) has **no FE caller** at all.
- **Provenance gap:** for anthropic with `provider_model=""`, the ledger's `model_requested` records empty even though the request carried `anthropic_model` (`mod.rs:182-183`, `pipeline.rs:596-611`).
- **Hidden per-provider behavior:** corpus budgets keyed on `provider_id` (ollama 3k vs 24k, `related_context.rs:37`; dossier 4k vs 80k, `commands.rs:2660`); agentic loops Cloud-backend-only; `provider_effort` honored by `anthropic` only (`anthropic.rs:61-71`).

### Intent → click-cost today

| User intent | Today | Traps hit |
|---|---|---|
| "Keep everything local" | 3-4 sections; result still not fully local (trap 3) | backend≠provider confusion |
| "Use my company LiteLLM gateway" | 3 sections / ~7 steps + invisible card + designed first-note failure | T1, model-picker inert, consent gap |
| "Best-quality notes (Opus)" | wrong door — the notes-model control hides under "grounded answers" copy | trap 2, T4 |
| "Cheap fast live + smart notes" | **impossible** — one `provider_model` drives everything | — |
| "Turn all cloud AI off" | 2-3 sections, incomplete; consent unrevokable | traps 3, 4 |
| "What model wrote this note?" | data exists (`pipeline.rs:595-612`) but no UI | — |

## Findings

### Angle 1 — prior-art patterns (web, all fetched 2026-07-02)

Pattern taxonomy across ~15 apps:

| Pattern | Apps | Fit for Murmur |
|---|---|---|
| A. Connections + one global default | Msty, TypingMind, Open WebUI, Jan | Too weak — Murmur already outgrew it (`brain_backend` ≠ `provider_id`) |
| B. Connections + registry + per-role assignment | Continue.dev (6 roles), Cursor, Smart Composer (3 slots) | Right semantics; full matrix = developer-grade decision fatigue |
| C. Opinionated, no picker | Granola, Notion AI (pre-2025) | Wrong — Murmur's choices are NOT privacy-equivalent; local-vs-cloud IS the product |
| **D. Hybrid: default + opt-in per-use overrides** | **Zed** (`default_model` + per-feature keys), Raycast, BoltAI ("Default" pointer) | **Best fit** — B semantics, C-like default experience |

Micro-UX worth stealing: **Jan's Local/Cloud split of the provider list** (v0.8.0 changelog — the redaction-firewall story rendered as IA); **Cherry Studio's "Check" = a real completion** (not a ping — Open WebUI's shallow verify is the anti-pattern); **fetch model lists where endpoints exist** (Ollama `/api/tags`, gateway `/v1/models`) with a free-text escape hatch (Murmur already does this for gateway, `settings.component.ts:1145`); **LM Studio/Jan hardware-fit badges** for the local GGUF picker; **Cursor's honesty about non-routable features** → Murmur's embeddings/NER/transcription shown as fixed "always on-device" badges; **BoltAI's override-as-pointer** (absence of override = follow default) — exactly Murmur's existing `""`-means-default convention, which avoids Raycast's documented staleness footgun (per-command picks don't follow default changes). Documented anti-patterns: Cursor decision-fatigue threads, Obsidian Copilot's keys-in-two-places confusion (issue #1235 — Murmur mirrors this smell today), Smart Connections' "Zero setup. No API key." as a competitive attack on key-first onboarding — the Obsidian audience rewards a local default that works before any provider is configured.

### Angle 2 — target architecture (roles over the existing seam)

**Connections stay implicit singletons** (`claude_code`, `anthropic`, `ollama`, `gateway`, plus `local` GGUF and `off`) — no named-connection registry now; a string `ConnectionId` keeps `"gateway:work"` reachable later without schema change. Connection config already exists (`claude_binary`+env toggle, Keychain `anthropic_api_key`, `ollama_base_url`, `gateway_base_url`+Keychain key).

**Roles — `Default` + 3 user-visible:**

```rust
pub enum Role { Notes, Ask, Live }
pub struct RoleTarget { connection: String, model: String, effort: String } // "" = inherit
pub fn resolve(role: Role, cfg: &AppConfig) -> RoleTarget;          // pure; legacy fallback
pub fn provider_for(role: Role, cfg: &AppConfig) -> Result<Arc<dyn SummarizerProvider>>;
// ReasonerCell::current() → current_for(role); CloudReasoner gains a role field
```

Only 3 roles visible; facts/graph/title inherit (no distinct call-site economics a user could reason about; a `title/meta` role has **no call site** — don't invent it). Embeddings/NER stay out of the dropdown (they'd be a lie until an Ollama-embeddings path exists). The **Notes dropdown offers only real `SummarizerProvider`s** — the GGUF doesn't implement the trait, and "local notes" already has a first-class answer: Ollama. No Mistral-as-SummarizerProvider adapter in v1.

**Fallback contract (the zero-breakage core):** with all role keys absent, `resolve(Notes)` = `(provider_id, provider_model, provider_effort)`; `resolve(Ask|Live)` = `brain_backend` mapped `cloud`→inherit, `local`→`("local", brain_model_id)`, `off`→`("off","")`. **No config rewrite at load, no one-shot migration write** — legacy keys become the resolver's bottom layer; an app downgrade still reads them untouched. 9 new additive KV keys: `role_{notes,ask,live}_{connection,model,effort}`.

**Call-site migration:** ~14 one-line mechanical edits (9× `make_provider(&config.provider_id, …)` → `provider_for(Role::X, …)`; 4× `ReasonerCell::current()` → `current_for(role)`; `all_providers` → `list_connections` backing) — lands in **one behavior-identical PR** provable by existing tests + a resolver identity-test matrix.

**IPC/FE:** the 9 role fields ride the existing `get_config`/`save_config`; new inbound-only commands `list_connections()` (reshape of `all_providers` + local + off) and `list_models(connection_id)` (gateway `/v1/models` exists at `commands.rs:3227`; **new** Ollama `GET /api/tags`; Claude ids move from the FE template `settings.component.ts:688-692` to a BE constant — single source of truth).

**Risks:** (1) redaction/consent bypass — structurally mitigated: classification, consent gate, and redaction wrap key off the **connection**, inside the factory, after resolution; add an adversarial test over every `(role, connection)` combo (consent OFF ⇒ refuse; ON ⇒ `RedactingProvider`-wrapped). `CloudReasoner` keeps its read-config-fresh-per-call discipline (the consent-revoke-egress fix, PR #116). (2) Ledger provenance must come from the resolved target — parameterizing `make_provider` fixes the existing anthropic provenance gap for free; test it. (3) Live-on-a-big-GGUF latency footgun — move the existing warning copy to the Live row. **Do NOT build:** per-folder overlays (deferred Phase 7 of the gateway plan, `docs/superpowers/plans/2026-06-30-ai-gateway-insights.md:730-745` — the resolver is its future seam), named multi-connections, per-role temperature/streaming.

### Angle 3 — target Settings IA

Sidebar 11 → **10 sections**: `Brain & AI` + `Providers` collapse into **"AI & Models"**; the provider dropdown leaves General; the Whisper-path override moves to Transcription (advanced disclosure).

```
AI & Models
├── A. PROVIDERS (connection cards, Local vs Cloud split, always all visible)
│     Claude Code ● Ready · "Cloud — redacted first"      [Configure ▾][Test]
│     Anthropic   ○ Needs key                              [Configure ▾][Test]
│     Ollama      ● Ready  · "On this Mac — nothing leaves"[Configure ▾][Test]
│     AI Gateway  ○ Not set up (today's :1108-1295 card, inline, unconditional)
├── B. WHAT MURMUR USES (assignment)
│     Default AI   [Claude Code ▾]  Model [Default ▾]   ← provider-aware model source
│       "Used for everything Murmur writes: notes, answers, digests, briefs."
│     ▸ Customize per feature (progressive disclosure)
│         Meeting notes     [Inherit default ▾]
│         Ask & assistant   [Inherit default ▾ | Local model | Off]  ← absorbs brain_backend
│           └ Local: today's GGUF registry block (+ hardware-fit badge)
│         Live in meetings  [Inherit default ▾ …] + voice-assistant / proactive toggles
│         On-device (fixed badges): semantic search + embed download, NER, transcription
└── C. WHERE YOUR TEXT GOES (privacy strip)
      ● Stays on this Mac: transcription, embeddings, name redaction, hints, local model
      ● Leaves (redacted first): {default connection} → {destination host}
      Cloud processing: Allowed ✓ [Revoke]        View activity →
```

**Copy discipline:** *Provider* = a connection that can run models (never "backend"); *Model* = a specific id on a provider; *Used for* = what an assignment powers. Ban "backend" user-facing and "brain" as a settings noun. Five worst labels rewritten (see IA-audit table — e.g. "Model / grounded answers" → "**Default model** — used for everything Murmur writes with AI"; "Assistant backend / Claude (cloud)" → "**Ask & assistant** — My default AI / A local model (on-device) / Off"). Fix the Polish string, the "Claude writes each summary" copy, the stale names-not-redacted paragraph, destination-aware consent banners (use `egress_is_cloud` via a tiny exposed command). Onboarding gains a gateway tile + consent-at-selection (kills the designed first-note failure).

**Structural prerequisite (measured):** `settings.component.ts` styles = **15.51 kB vs 16 kB error budget** (`ng build`, 2026-07-02); 3,713 lines. Split into a shell + per-section children first (watch the `forwardRef`/NG0600/opaque-overlay traps per `angular-zoneless.md`).

## Fit with Murmur's constraints

- **Local-first / privacy:** strengthened — the IA itself becomes the privacy disclosure (Jan-style Local/Cloud split + strip); the consent-banner classification gap (gateway/remote-ollama) gets closed; consent + redaction remain per-connection inside the factory, invariant under roles; `list_models` calls are inbound-only per the `list_gateway_models` precedent. Ask→local GGUF becomes an honest, selectable privacy gain.
- **Provider seam + redaction firewall:** strictly preserved — roles parameterize `make_provider`; nothing bypasses it; `ReasonerCell` keeps live-config fail-closed posture.
- **SQLite-canonical / additive migrations:** all new config = additive KV keys; no `Db::migrate()` change; legacy keys never rewritten (downgrade-safe).
- **Lock model:** untouched — no new content read path (`list_connections`/`list_models` return config metadata only).
- **CI honesty:** fully headless-verifiable (pure resolver tests, Playwright + mocked invoke for the hub); the only real-Mac item is eyeballing the packaged WKWebView render of the split components (the T4 CSP lesson).

## Options & tradeoffs

| # | Option | Effort | Risk | Unlocks |
|---|---|---|---|---|
| 0 | **Copy-only triage**: fix the 4 false help texts + Polish string + stale privacy copy; gate the Model dropdown on provider; hint gateway in Providers | S (~1 day) | ~0 | Kills the worst lies immediately; no component split needed |
| 1 | **Settings component split** (shell + per-section children, behavior-identical) | S/M | low (FE traps known) | Clears the 15.51/16 kB cliff; prerequisite for everything below |
| 2 | **AI & Models hub** (Blocks A+B-simple+C; dissolve Providers; move dropdown; onboarding gateway tile + consent-at-selection; `revoke_cloud_egress` command; one provider-aware Model control replacing `anthropic_model` free-text) | M (~3-5 days) | moderate FE | Kills T1/T4/traps 1-2 for the default path; consent coherent |
| 3 | **Role resolver backend** (`resolve`/`provider_for`/`current_for` + 14 call-site swaps + identity/consent/provenance tests) — behavior-identical | S/M (1 PR) | near-zero | THE architecture; also fixes ledger provenance |
| 4 | **Per-feature override UI** (Notes/Ask/Live rows on the hub; 9 role keys; `list_models(ollama)`; `brain_backend` UI absorbed) | M | moderate | "Cheap live / smart notes"; honest Local semantics; trap 3/4 fixed |
| — | Full Continue-style registry / named multi-connections / per-folder overlays | L | high | Nothing demanded today — skip (seams preserved) |

## Recommendation & first step

**Ship 0 → 1 → 2 → 3 → 4 as a staged sequence** (0 can ride with anything; 3 can run in parallel with 1-2 since it's backend-only and behavior-identical). All four research angles independently converged on this shape: consolidate first (the hub), land the resolver as an invisible seam, then expose exactly three per-feature rows with "Inherit default" pointers.

**Smallest verifiable first slice:** the copy/logic triage (option 0) + a spike of `resolve()` alone with its identity-test matrix against current `AppConfig` (hours — proves the fallback contract has no ambiguity around `provider_model`'s dual use before anything else is built). Both are live-verifiable headless.

**Needs a product decision from the user (not more research):**
1. Consent **revoke** — the grant-only design was deliberate (`settings.component.ts:3505-3510`); the privacy strip wants a Revoke button.
2. Whether typed @brain threads count as `Live` or `Ask` (both funnel through `run_assistant_query`; splitting needs a role param through that path — defaulted to `Live` here).
3. Whether "Off" should also force `realtime_reactions` off (today's trap 4) or the assistant should honestly say "deterministic answers only".

## Open questions / what couldn't be verified

- Runtime look of `brain_backend=Off` + `realtime_reactions=ON` (stub-floor answers) — needs a live session.
- Per-child style sizes after the settings split — estimates until built; packaged-WKWebView render needs a notarized build.
- Anthropic public list-models endpoint vs a BE constant — constant is fine for v1.
- Cherry Studio / Open WebUI micro-details are docs-search confidence (medium); Granola's Business-tier models are from a third-party review.
- The unmerged `worktree-ai-gateway-insights` branch (fuller egress ledger) wasn't inspected; Block C's "View activity" assumes trunk's `egress_log::active_sink` rows become queryable (sink exists, `mod.rs:192`; no read command yet).

## Sources

**Code (all under `/Users/jakubgawronski/Projects/meetnotes/`):**
- `src-tauri/src/summarize/mod.rs:61-247` (egress_is_cloud, make_provider, ledger, all_providers); `provider.rs:43-120`; `claude_code.rs`, `anthropic.rs`, `ollama.rs`, `gateway.rs` (per-arm model handling)
- `src-tauri/src/settings/config.rs:22-30,54-224,277-314,320-554` (BrainBackend, fields, KV load/save)
- `src-tauri/src/reason.rs:54-86,165-179,259-329,506-586` (BRAIN_MODELS, ReasonerCell, CloudReasoner)
- `src-tauri/src/pipeline.rs:550,572,595-625,652,883`; `src-tauri/src/commands.rs:716,1247,1490,1775,1857,1948-2203,2615-2699,2831-2854,3062-3155,3227,3587,3800-3911,4148`
- `src-tauri/src/transcribe/live.rs:301-465,1090`; `embed.rs:146`; `summarize/redact.rs:79`; `summarize/related_context.rs:37`; `mcp.rs:341,670`; `proactive.rs`
- `src/app/features/settings/settings.component.ts:42-54,216-1362,1402-1459,3182-3510` (full section inventory, gateway gate, no-revoke)
- `src/app/features/onboarding/onboarding.component.ts:17-31,1124-1370`; `src/app/features/record/record.component.ts:232-255,1104-1318`
- `docs/superpowers/plans/2026-06-30-ai-gateway-insights.md:730-745` (deferred per-folder Phase 7)
- `ng build` (2026-07-02): settings styles 15.51 kB vs 12.29 kB warn / 16 kB error

**Web (fetched 2026-07-02 unless noted):**
1. https://docs.continue.dev/customize/model-roles + https://docs.continue.dev/reference — role taxonomy + config schema
2. https://cursor.com/help/models-and-usage/api-keys — Verify button, Tab/Apply exclusion, BYO-key privacy warning
3. https://zed.dev/docs/ai/agent-settings — default_model + per-feature override keys with inherit fallback
4. https://manual.raycast.com/ai/ai-commands — default + per-command override + staleness caveat
5. https://www.jan.ai/changelog/2026-05-22-jan-v0.8.0 + 2026-05-29-jan-v0.8.1 — Local/Remote provider split, hardware-fit labels, Add Provider dialog
6. https://www.obsidiancopilot.com/en/docs/settings + https://github.com/logancyang/obsidian-copilot/issues/1235 — 5-tab IA, keys-in-two-places confusion
7. https://github.com/glowingjade/obsidian-smart-composer — Chat/Apply/Embedding trio
8. https://github.com/brianpetro/obsidian-smart-connections — "Zero setup. No API key." positioning
9. https://docs.boltai.com/docs/ai-command/customize-an-ai-command — "Default" pointer convention
10. https://docs.cherry-ai.com/docs/en-us/cherry-studio/preview/settings/providers — Check = real completion (medium conf.)
11. https://docs.openwebui.com/getting-started/quick-start/connect-a-provider/ — auto-fetched models, shallow verify caveat
12. https://docs.typingmind.com/chat-models-settings/set-up-custom-models — Test-before-save
13. https://lmstudio.ai/models + https://lmstudio.ai/blog/lmstudio-v0.3.5 — fit badges, per-model defaults
14. https://venturebeat.com/ai/notion-bets-big-on-integrated-llms-adds-gpt-4-1-and-claude-3-7-to-platform (May 2025) — even opinionated apps drifted toward a picker
15. https://tldv.io/blog/granola-review/ — Granola model opacity (secondary)
16. https://forum.cursor.com/t/model-selection-usage-limits-are-becoming-stressful/151100 (+ /t/80782, /t/72037, /t/54155) — decision-fatigue + registry-drift anti-patterns
