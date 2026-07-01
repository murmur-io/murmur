<!-- Generated 2026-06-30 via /research (murmur-researcher fan-out, 4 angles). Kong pricing/licensing/version facts are point-in-time (June 2026) — re-verify before quoting later. -->
# Research: Kong AI Gateway — fit, friction, and the genuinely good ideas for Murmur

## TL;DR / Verdict

**Don't bundle Kong (no). Steal 2-3 of its ideas as ~100-line native Rust decorators (yes). Keep Kong on the shelf as the egress-governance layer for a future Murmur-for-Teams tier (the real long game, Phase-3-distant).**

Kong AI Gateway is not an embeddable library — it's a set of Lua plugins on a server-grade OpenResty/Nginx daemon (recommended ~500 MB RAM/worker, Docker-oriented, `anonymous_reports` telemetry **ON by default**). The decisive facts, all confirmed from primary sources and convergent across four independent research passes:

1. **The free OSS plugins are exactly what Murmur already owns in-process.** Only six AI plugins are Apache-2.0 (`ai-proxy`, `ai-prompt-guard`, `ai-prompt-template`, `ai-prompt-decorator`, `ai-request/response-transformer`) — a normalizing LLM proxy + a regex guard + prompt templating. That is precisely our `SummarizerProvider` trait + `RedactingProvider` firewall.
2. **Every *valuable* AI plugin is Enterprise-licensed** (`tier: ai_gateway_enterprise`): multi-LLM fallback/circuit-breaking, semantic cache, PII sanitizer, RAG injector, token rate-limiting, prompt compressor. Several also require a vector DB + an external embeddings service + (for the PII sanitizer/compressor) a *private Docker image you must email Kong to obtain*.
3. **A gateway only intercepts HTTP — and Murmur's *default* egress isn't HTTP.** The default provider `claude_code` spawns the `claude` **CLI subprocess**; a perfectly-placed gateway sees **~0 % of a default install's content egress**. Only the non-default, BYO-key `anthropic` provider is interceptable, and its URL is hardcoded.
4. **Our provider mix (CLI + HTTP API + local server) is strictly *more* capable than a gateway** for failover: a gateway can only unify HTTP endpoints, so it physically cannot fail over from the `claude` CLI to local `ollama`. Our trait seam can.

So routing Murmur through Kong would replace clean in-process Rust with a heavyweight second daemon, cover a minority of egress, duplicate three things we already ship, and unlock only the paywalled bits. **The genius isn't "use Kong" — it's "Kong is a beautifully-documented reference architecture; build the 2-3 patterns worth having as small features on the seam we already have."**

---

## What we already have (grounded in the tree)

| Capability | Murmur today | file:line |
| --- | --- | --- |
| **One swappable provider seam** (= Kong "AI Proxy") | `trait SummarizerProvider { id / availability / summarize / complete }`, built by `make_provider(id, config)` | `summarize/provider.rs:42`, `summarize/mod.rs:63` |
| **Pre-egress PII firewall** (= Kong Enterprise "AI Sanitizer") | `RedactingProvider` wraps any provider; scrubs emails/cards/phones (regex) + optional on-device DeBERTa NER for names; de-tokenizes the reply; runs **on-device before egress** | `summarize/redact.rs:278`, wired at `mod.rs:125` |
| **Fail-closed cloud-egress consent gate** | inside `make_provider`; `is_cloud()` treats `claude_code` + `anthropic` as cloud, `ollama` exempt | `summarize/mod.rs:53,69` |
| **In-process, lock-gated RAG injection** (= Kong Enterprise "AI RAG Injector") | `build_related_context` pulls related notes from SQLite via `visibility_clause`-gated `search_visible`/`get_note_if_visible`, injects into the prompt | `summarize/related_context.rs:147`, `summarize/vault_context.rs:84` |
| **Agentic brain routes *through* the seam** (no bypass) | `CloudReasoner` "holds NO reqwest/HTTP client", builds its provider via `make_provider`; no direct `reqwest` in `agent.rs`/`orchestrate.rs` | `agent.rs:73`, `reason.rs:417,468` |
| **A "BYO gateway" hatch already exists, opt-in** | `claude_code_inherit_env=true` lets the `claude` subprocess inherit `ANTHROPIC_BASE_URL`/`HTTPS_PROXY`; the error copy already names "a proxy / LiteLLM endpoint" | `settings/config.rs:201`, `summarize/claude_code.rs:484` |

**What we do NOT have** (grep across `summarize/`, `agent.rs`, `reason.rs`, `orchestrate.rs`, `pipeline.rs` for `retry|fallback|circuit|failover|cache` → **zero hits**): no automatic provider fallback, no circuit-breaking, no response/semantic cache, no token-budget governance, and **no egress audit log** (the consent gate is a single boolean at `commands.rs:1987`, not a record of *what was sent*).

---

## Findings (per angle)

### 1. What Kong AI Gateway actually is (footprint + licensing reality)
- Plugins on **Kong Gateway = Nginx/OpenResty + LuaJIT**, configured via Admin API or a declarative file [1][2]. Server-grade: Kong recommends **~500 MB RAM/worker, 1 worker/core**; the AI sizing guide baselines 1 vCPU : 2 GB [15][17]. Distributed primarily as a Docker image — not linkable into a Rust binary. (high)
- **OSS vs Enterprise split — load-bearing, confirmed from Kong's own `constants.lua` source [12]:** only six AI plugins are OSS (proxy, prompt-guard, prompt-template, prompt-decorator, request/response-transformer). **Enterprise-only:** `ai-proxy-advanced` (fallback/LB/circuit-breaker), `ai-semantic-cache`, `ai-semantic-prompt-guard`, `ai-rag-injector`, `ai-sanitizer` (PII), `ai-prompt-compressor`, `ai-rate-limiting-advanced` [3][5][7][8][9][10][11]. (high)
- **Semantic Cache / Prompt Guard / RAG need a vector DB (Redis/pgvector) + an embeddings provider** (the documented default embeddings model is a **cloud** OpenAI call) [7][8][9]. (high)
- **PII Sanitizer runs locally but as a separate "AI PII Anonymizer" Docker container** from a private registry [10] — privacy-respecting, but Enterprise + a second container. The free `ai-prompt-guard` is **regex allow/deny, not PII redaction** [4]. (high)
- **Telemetry: `anonymous_reports` defaults `on`** (sends usage data/stack traces to Kong); must be explicitly disabled [16]. Out-of-the-box it phones home. (high)
- Lightest self-host = **DB-less/declarative** (no Postgres/Cassandra) [14] — but its Admin API is read-only and the Enterprise semantic plugins need Redis/pgvector anyway, so they're not "DB-less" in spirit. (high)

### 2. Fit & friction with Murmur's architecture
- **Complete cloud-HTTP egress inventory of the tree:** the only interceptable LLM content egress is `anthropic.rs:8,149,216` (hardcoded `https://api.anthropic.com/v1/messages`). `claude_code` is a CLI subprocess (Murmur never opens that socket); `ollama` is localhost; Brave web search rides its own consent-gated `connectors/web.rs:107` seam; HF model downloads are weights, not content. (high — read the source)
- **Quantified coverage:** because the **default provider is `claude_code`**, a gateway sees **~0 %** of a default install's content egress, and even for `anthropic` you'd first have to make the URL configurable. (high)
- **Redundancy-vs-gap table:**

  | Kong capability | Murmur equivalent | Verdict |
  | --- | --- | --- |
  | PII sanitizer / prompt-guard | `RedactingProvider` + DeBERTa NER | **Redundant** — and better-positioned (on-device, before egress) |
  | Multi-LLM proxy/abstraction | `SummarizerProvider` + `make_provider` | **Redundant** |
  | RAG injection | `build_related_context` (SQLite, lock-gated) | **Redundant** — and Kong's would require shipping the vault to its vector DB |
  | Automatic fallback / circuit-breaking | none | **Genuine gap** (Enterprise in Kong) |
  | Semantic / response cache | none | Gap, but Enterprise + needs vector DB + cloud embeddings |
  | Token-budget governance | none | Gap, but **marginal** for a single user |
  | Egress observability / audit | none (consent is a binary flag) | **Genuine gap** — cheap, on-brand |

- All three genuine gaps plug in at the **same seam** (`mod.rs:125`) as `RedactingProvider`, as `Arc<dyn SummarizerProvider>` decorators. (high)

### 3. Alternatives + is a gateway even the right *shape*?
- **Category breakeven is far above us:** "If you call a single provider… a direct SDK call is simpler… the trade tips toward a gateway once you pass a couple of developers or a few thousand requests a day. Below that, direct calls usually win." [Infrabase, med-high]. Murmur is one user, a few calls per meeting.
- **Alternatives:** LiteLLM (MIT, but a **Python service + Redis** = a second daemon); Portkey (Apache-2.0 gateway, useful persistence is SaaS); **Cloudflare AI Gateway / OpenRouter = SaaS-only → privacy-incompatible** as infra (only usable *as just another cloud provider behind our firewall*); Helicone (Rust, fast — but **acquired by Mintlify, now maintenance-mode**); Apache APISIX (same Nginx/OpenResty weight as Kong). **Rust-native reference designs that prove fallback is a small library concern, not infra:** `llm-cascade` (ordered fallback + **SQLite cooldowns** + 429 backoff) and `rust-genai`. (med-high)
- **The sharp architectural fact:** our provider mix spans a **CLI** (`claude_code`), an **HTTP API** (`anthropic`), and a **local server** (`ollama`). A gateway unifies only HTTP endpoints → it cannot fail over from the `claude` CLI to `ollama`. **The trait is strictly more capable than a gateway for our specific mix.** (high)
- **What comparable apps do:** the verifiable local-first comparable (Obsidian Copilot, OSS) calls provider APIs **directly** + an OpenAI-compatible base-URL field — **no gateway/proxy/cache infra** [Copilot repo]. Granola/Reflect are closed cloud SaaS → not privacy comparables; not inferred. The prevailing privacy-first pattern is exactly our thin provider abstraction. (high for the OSS comparable)
- **Moat read (blunt):** "Murmur routes through Kong" is **invisible plumbing** — never a user-visible differentiator. The moat is the *integration* (local-first + Obsidian-native + redaction + meeting-memory), not the pipe.

### 4. Capability → real Murmur pain, and the "where it's genius" inversion
- **Fallback/circuit-breaking ↔ shipped `claude_code` regressions — the strongest map.** We have hand-patched this class repeatedly: `ab413c6` (model-picker → `--model claude-sonnet-4-6` → opaque "claude exited with status 1" on a proxy that doesn't know that id), `efef9c7` (`env_clear` broke shell-`ANTHROPIC_API_KEY` auth), both remediated in **`d3b13b3` (v0.6.1)** with an actionable error. Today a flaky `claude_code` just *fails*; nothing auto-recovers. **Sharp refinement:** two of three regressions are *intra-*`claude_code`, so the chain should support intra-provider hops too: `claude_code(--model X)` → `claude_code(no --model)` → `anthropic` → `ollama`. The first hop alone would have auto-recovered `ab413c6`. (high — git + code)
- **Egress audit ↔ a "privacy receipt".** Kong ships fleet usage reporting; we don't need fleet metrics — we need a **per-user privacy ledger**: "what left my Mac, to whom, and what we scrubbed first." `COMPETITIVE-LANDSCAPE.md:55` notes the redaction firewall is already our strongest, *uncontested* differentiator — a receipt makes it **visible and provable**. (high)
- **PII defense-in-depth ↔ a real, self-admitted gap.** `redact.rs:1-6` states the regex layer catches emails/cards/phones only; names need NER, which is **download-not-bundled and off on a clean install** (`active_name_redactor()` returns the no-op when the model is absent, `redact.rs:76`). We miss SSN/IBAN/IP/crypto-address/passport. Broaden **natively** — never via Kong's sanitizer, which *sends content to an external service* (the exact thing our firewall exists to prevent). (high)
- **Semantic cache / token rate-limit / RAG injection ↔ no real single-user pain.** Summaries persist once in SQLite (re-summarize is rare); the timeline side-task already caches (`348341d`); single-user prompts are near-unique → low hit rate. Rate-limiting is moot (own subscription / own key / free local). RAG we already do, lock-gated. These only light up in the Teams inversion. (med-high)
- **Two adversarial privacy hazards a naive fallback MUST encode** (a gateway gets these free as a stateless proxy; we don't): (1) **egress-class escalation** — silently failing over from local-only `ollama` to a cloud provider leaks content the user kept on-device; stay within the same egress class unless the user explicitly configures a cross-class chain *and* consent is re-checked; (2) **double-send** — only fail over when the call *failed before egress completed*; never re-send already-delivered content.

---

## Fit with Murmur's constraints

- **Local-first / privacy.** Bundling Kong **fails hard**: its semantic cache defaults to a *cloud* embeddings call and its PII/RAG plugins add network hops + external services; `anonymous_reports` phones home by default; Konnect SaaS is an outright violation. The native decorators are the opposite — a `FallbackProvider` adds **zero new egress class** (each arm is already `RedactingProvider`-wrapped + consent-gated), and the receipt is a pure local SQLite write.
- **Obsidian-native / SQLite-canonical.** A cache/audit belongs in **our SQLite** (one source of truth, SQLCipher-at-rest, additive guarded migration), not a gateway's Redis/pgvector — which would be a divergent copy of the truth (violates constraint 3).
- **Provider seam + redaction firewall intact.** Both native ideas *ride* the seam exactly like `RedactingProvider`; they compose `Arc<dyn SummarizerProvider>`, so every cloud arm stays redacted + consent-gated by construction.
- **No-PII-in-logs (rust-tauri §8).** The receipt stores provider id + meeting id + byte counts + PII-token **counts by kind** + model — **never** scrubbed values or transcript. Easy to honor; must be honored.
- **macOS-first / single-binary / CI honesty.** Bundling an OpenResty daemon (+ optional Postgres/Redis + per-language PII Docker containers) inside a **notarized** `.app` is a major sign/notarize/supervise burden (we already manage 3-4 Swift sidecars and have been bitten by unsigned helpers failing notarization). The native decorators ship in the existing universal binary, fully unit-testable under `cargo test --lib` with the existing `EchoProvider`/`CaptureProvider` mock harness (`redact.rs:409+`) — **no "needs a real Mac" caveat.**

---

## Options & tradeoffs

- **A — Native `FallbackProvider` (provider fallback + circuit-breaker).** Effort **S-M**, risk **low-med**. Ordered `Vec<Arc<dyn SummarizerProvider>>`, first `Ok` wins, per-arm `max_fails`→cooldown; fail over only on *retryable* errors (non-zero exit/timeout/`Unavailable`), **not** on client/validation errors (mirror Kong's "client errors don't trigger failover"); support intra-`claude_code` hops. Unlocks resilience for the exact bugs we keep patching. Risk = the two privacy hazards above; keep the *default* chain single-provider (opt-in to a chain) so existing behavior stays byte-identical. **← recommended first.**
- **B — Local egress "privacy receipt" (audit ledger + UI).** Effort **S-M**, risk **low**. Additive `egress_log` table + an append inside `RedactingProvider` (counts, not content) + a small read-only Angular panel. Unlocks the most on-brand local-first feature available — makes our strongest differentiator *visible and provable*; also quantifies the `claude_code` failure rate. **← recommended high-value follow-on.**
- **C — Broaden PII coverage natively (defense-in-depth).** Effort **S** (more regex classes) to **M** (default-on NER + confirm-before-egress review). Closes the self-admitted firewall gap without Kong's external-service model. Risk: a new redactor mangling a legitimate number/string — ship behind the existing no-regression discipline. **Defer / opportunistic.**
- **C′ — One-line `anthropic_base_url` config (BYO gateway).** Effort **S**, risk **low**. Make `anthropic.rs:8` configurable so the 1 % who already run LiteLLM/Kong can point the HTTP provider at it (the `claude_code` path already supports this via `ANTHROPIC_BASE_URL`). Zero bundling, serves power users, pairs with A. **Cheap bonus.**
- **D — Bundle/require Kong (or APISIX/LiteLLM daemon).** Effort **L**, risk **high**. OpenResty/Lua (± Postgres/Redis + PII Docker) inside a notarized Mac app; Enterprise license for the only useful plugins; covers ~0 % of default egress; invisible to the user; wrong category. **Reject — on the record.**

---

## Recommendation & first step

**Do A, then B. Add C′ as a cheap bonus. Defer C. Reject D. File "AI gateway" as a deferred Teams-tier seam.**

**Smallest verifiable first slice — a unit-tested `FallbackProvider` spike (an afternoon, no Mac, no network):** add the decorator wrapping two mock arms and assert, RED-before-GREEN, that (i) a `Summarize` error on arm 1 transparently yields arm 2's result; (ii) a *client/validation* error does **not** fail over; (iii) the default single-provider config is byte-identical to today; (iv) a local-only arm never silently escalates to a cloud arm without consent. Reuse the `EchoProvider`/`CaptureProvider` doubles in `redact.rs:409+`; verifiable with `cargo test --lib`. That de-risks the entire "valuable bits without infra" thesis and directly attacks the one Kong capability with real, evidenced user value. If it lands, B (the privacy receipt) is the marketing-grade follow-on, and a SQLite-backed exact-hash response cache is the natural third.

---

## The inversion — where Kong is genuinely genius

**For a future Murmur-for-Teams / enterprise tier, every "no real pain" cell flips to "yes" once there are many seats and one org bill** — and that's exactly Kong's sweet spot. This maps onto the *already-specced-but-deferred* `docs/ARCHITECTURE-LOCAL-CLOUD.md` **Feature 5** (`TeamBrainProvider` arm + hosted MCP + zero-knowledge sync, Phase 3, effort L). When org users' *already-client-redacted* egress is centrally governed, an AI gateway delivers the enterprise checklist: shared key vault (one org account vs N BYO keys), per-seat token budgets + cost governance, org-wide PII policy as a belt-and-suspenders *second* layer, audit + cost dashboards, model allow-lists, fleet failover. It sits in front of the **hosted-inference arm only — never the local single-user app.**

**Three honest caveats that keep this from being overhyped:** (1) it's **Phase-3-distant** (gated behind a sync substrate + multi-device, `ARCHITECTURE-LOCAL-CLOUD.md:142`); (2) Kong covers only ~**one-third** of Feature 5 — it governs LLM egress, not the hosted-MCP-over-ACL or the E2E zero-knowledge sync relay; (3) it's **not uniquely Kong** — LiteLLM/Portkey/Bedrock/Azure fill the same role, and Kong wins specifically *if the customer org already runs Kong as its API gateway*. So the durable framing is *"an AI gateway for the Teams tier, plausibly Kong"* — not *"Kong is the Teams tier."*

---

## Open questions / what couldn't be verified

- **User provider mix.** `claude_code` is the *default* (`mod.rs:36`), but Murmur ships no telemetry by design, so the fraction of users on `anthropic` (the only egress a gateway could ever cover) is unknowable.
- **Kong pricing/licensing** (~$105/mo/service Konnect entry, $50k+/yr mid-size) is point-in-time June-2026 and partly third-party; the *fact* that the valuable plugins are Enterprise-tier is vendor/source-confirmed, the dollar figures less so.
- **Idle RAM of one DB-less Kong on Apple Silicon** — recommended sizing (~500 MB/worker) read, not a measured idle figure.
- **Whether the `claude` CLI honors `ANTHROPIC_BASE_URL`** for repointing at a gateway — we pass it through under `inherit_env`; not independently end-to-end tested against a running LiteLLM (needs a real Mac + a gateway instance).
- **Semantic-cache hit-rate** for meeting summaries / Ask-My-Vault is reasoned from prompt-uniqueness, not measured; revisit if Teams shows repeated near-identical org questions.
- **Point-in-time facts to re-verify before quoting later:** Portkey Apache-2.0 since Mar 2026; Helicone acquired by Mintlify / maintenance-mode; Kong's `anonymous_reports` payload manifest unread; the *base* AI Proxy lacking fallback is inferred from the Advanced doc owning failover.

---

## Sources

**Code (this repo):**
- `src-tauri/src/summarize/provider.rs:42,54` — `SummarizerProvider` trait (the seam).
- `src-tauri/src/summarize/mod.rs:36,53,63,69,125` — default provider id, `is_cloud`, `make_provider`, consent gate, redaction wrap.
- `src-tauri/src/summarize/anthropic.rs:8,149,216` — hardcoded Anthropic URL; the only HTTP content egress; single POST, no retry.
- `src-tauri/src/summarize/claude_code.rs:36,333,484` — CLI subprocess; env hardening; 180s timeout; actionable error naming a proxy/LiteLLM endpoint.
- `src-tauri/src/summarize/ollama.rs:10,19` — localhost-only provider.
- `src-tauri/src/summarize/redact.rs:1-6,76,278,409+` — firewall self-admitted regex-only scope; NER no-op when model absent; `RedactingProvider`; mock-provider test harness.
- `src-tauri/src/summarize/related_context.rs:147`, `vault_context.rs:84` — in-process lock-gated SQLite RAG.
- `src-tauri/src/agent.rs:73`, `reason.rs:417,468`, `orchestrate.rs:154` — brain routes through `make_provider`, no direct HTTP; failure → deterministic local floor, never another provider.
- `src-tauri/src/settings/config.rs:201` — `claude_code_inherit_env`/`ANTHROPIC_BASE_URL` passthrough.
- `docs/ARCHITECTURE-LOCAL-CLOUD.md:91-97,142-145` — Feature 5 / `TeamBrainProvider` / hosted MCP (the inversion target, Phase-3 deferred).
- `docs/COMPETITIVE-LANDSCAPE.md:55` — redaction firewall = strongest uncontested differentiator (supports the privacy receipt).
- Git: `ab413c6` (model-picker regression cause), `efef9c7` (env_clear regression cause), `d3b13b3` (v0.6.1 actionable error), `348341d` (timeline caching).

**Kong / external (point-in-time June 2026):**
1. https://developer.konghq.com/ai-gateway/ — overview: plugins on Kong Gateway; deployment modes; Konnect SaaS control plane.
2. https://github.com/Kong/kong — "The API and AI Gateway" (OpenResty/Nginx+LuaJIT).
3. https://developer.konghq.com/plugins/ai-rate-limiting-advanced/ — token-based rate limiting (Enterprise).
4. https://konghq.com/blog/product-releases/announcing-kong-ai-gateway — Feb 2024 launch; the six OSS AI plugins.
5. https://developer.konghq.com/plugins/ai-proxy-advanced/ — fallback/load-balancing/circuit-breaker; `tier: ai_gateway_enterprise`.
6. https://developer.konghq.com/plugins/ai-proxy/ — base proxy; ~16 providers incl. Ollama/Anthropic; no embeddings needed.
7. https://developer.konghq.com/plugins/ai-semantic-cache/ — Enterprise; needs vector DB + embeddings; caches in the vector DB.
8. https://developer.konghq.com/plugins/ai-semantic-prompt-guard/ — Enterprise; needs embeddings + vector DB.
9. https://developer.konghq.com/plugins/ai-rag-injector/ — Enterprise; needs vector DB + embeddings.
10. https://developer.konghq.com/plugins/ai-sanitizer/ — Enterprise; separate local "AI PII Anonymizer" Docker container; ~18-20 categories.
11. https://developer.konghq.com/plugins/ai-prompt-compressor/ — Enterprise; requires a private Docker image (contact Kong); LLMLingua 2.
12. https://raw.githubusercontent.com/Kong/kong/master/kong/constants.lua — authoritative OSS bundled-plugin list (only the six free AI plugins).
13. https://www.truefoundry.com/blog/kong-gateway-pricing-architecture-an-analysis-for-ai-teams-2026-edition — third-party 2026 pricing/footprint analysis.
14. https://developer.konghq.com/gateway/db-less-mode/ — DB-less: single declarative file, read-only Admin API, not all plugins compatible.
15. https://developer.konghq.com/gateway/resource-sizing-guidelines/ — ~500 MB RAM/worker, 1 worker/core.
16. https://developer.konghq.com/gateway/configuration/ — `anonymous_reports` defaults `on`.
17. https://developer.konghq.com/ai-gateway/resource-sizing-guidelines-ai/ — AI sizing 1 vCPU : 2 GB baseline.
18. https://infrabase.ai/blog/ai-gateways-explained — direct-SDK-vs-gateway breakeven (category-fit argument).
19. https://github.com/BerriAI/litellm + https://docs.litellm.ai/docs/simple_proxy — LiteLLM MIT proxy; fallbacks/retries/Redis cache (a Python daemon).
20. https://crates.io/crates/llm-cascade (repo paluigi/llm-cascade) + https://github.com/jeremychone/rust-genai — Rust in-process fallback reference designs (fallback = a small library concern, not infra).
21. https://github.com/logancyang/obsidian-copilot — local-first Obsidian AI: direct provider APIs + base-URL, no gateway (prevailing pattern).
22. https://techsy.io/en/blog/best-llm-gateway-tools + https://www.truefoundry.com/blog/helicone-vs-portkey — Portkey/Helicone/Cloudflare survey (SaaS-vs-self-host, Helicone maintenance-mode).
23. https://openrouter.ai/docs/faq — OpenRouter is a SaaS proxy that sees prompts.
24. https://apisix.apache.org/docs/apisix/plugins/ai-proxy-multi/ — APISIX fallback/retry/health (same OpenResty weight as Kong).
