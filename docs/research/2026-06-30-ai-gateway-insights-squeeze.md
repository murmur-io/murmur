<!-- Generated 2026-06-30 via /research (murmur-researcher fan-out, round 2 / 4 angles). Builds on 2026-06-30-kong-ai-gateway-fit.md. Gateway field names/headers are point-in-time June 2026 — re-verify before coding. -->
# Research: AI Gateway — squeezing the MOST out of it (provider + "Gateway Insights")

> Round 2. Round 1 (`2026-06-30-kong-ai-gateway-fit.md`) decided: don't bundle Kong; add a first-class OpenAI-compatible "AI Gateway" provider + build features that read the metadata a gateway returns. This round maps EXACTLY what to read, what to build, the security guardrails, and the two surprises found in our own code.

## TL;DR / Verdict

**Build "Gateway Insights" — but the real unlock is "stop discarding metadata we already receive," and there's a live egress gap to close along the way.** Two findings reframe everything:

1. **The highest-value data is already on the wire and thrown away.** Our own `anthropic` provider receives `usage` (token counts) + a `model` echo on every call and discards both (`anthropic.rs:91-97` only deserializes `content`+`stop_reason`). The "usage receipt" needs *no gateway at all* for anthropic users — a gateway just extends the same data to the LiteLLM/OpenRouter crowd and adds `/v1/models` + a trace id.
2. **A user-supplied base URL already ships — unsafely.** `ollama_base_url` is free-text we POST transcript to, and `ollama` is classified non-cloud, so that path **bypasses BOTH the redaction firewall AND the consent gate** (`mod.rs:102-107` returns before the `RedactingProvider` wrap at `:125`). Today a user can point it at any cloud URL and raw, unredacted transcript leaves with no consent. The new provider must close this, not widen it — and the `ollama_base_url` hole deserves a `lock-security-reviewer` pass regardless.

**Recommended bundle (2 tiers + a capability fast-follow):**
- **Tier 1 (now, S, low risk):** the gateway provider with the **security guardrails baked in** (treat any gateway URL as cloud → redact + consent, even localhost; https-enforced; key bound to URL) + **model-catalog picker from `/v1/models`** + **per-note model provenance in frontmatter** + **structured gateway errors + health dot**.
- **Tier 2 (the wedge, M):** a unified **"Egress & Usage Ledger"** in Analytics that fuses round-1's privacy receipt with token-usage receipts — token-denominated (NOT currency, per `rust-tauri §10`) — plus **per-task / per-folder model profiles** (sensitive folder → local model = a privacy *gain*).
- **Capability fast-follow:** **structured-output (`json_schema`)** to harden the two JSON side-tasks (cheap), then **token streaming** (the long-deferred feature — gateway-first, blocked only by a firewall de-tokenizer).
- **Skip:** native function-calling (our gated executor is safer), gateway moderation (wrong side of the firewall), embeddings-via-gateway (dim-mismatch + privacy regression), currency/$ dashboards (§10).

The spine of the whole thing is the `RedactingProvider` chokepoint (`redact.rs:278`): every insight is computed there, the one place every cloud call converges — so it stays local-first and consent-gated by construction.

---

## What we already have (grounded — incl. the two surprises)

| Thing | State | file:line |
| --- | --- | --- |
| **`anthropic` receives `usage` + `model` echo and DISCARDS them** | shipped (surprise #1) | `summarize/anthropic.rs:91-97` (`MessagesResponse` = `content`+`stop_reason` only) |
| **`ollama_base_url` = user-typed URL we POST transcript to, NOT redaction-wrapped, NOT consent-gated** | shipped (surprise #2 / live gap) | `summarize/config.rs:77`; `summarize/mod.rs:54,102-107` (returns before the wrap at `:125`) |
| Provider seam returns bare `Result<String>` — no channel for usage/model/headers | shipped | `summarize/provider.rs:50,54` |
| `is_cloud()` hardcoded `matches!` over two ids (the one-liner that forces firewall+consent) | shipped | `summarize/mod.rs:53` |
| `make_provider` builds + wraps cloud providers in `RedactingProvider` + consent gate | shipped | `summarize/mod.rs:63,69,125` |
| `claude_code` runs plain `-p` (no `--output-format json` → no usage), default provider | shipped | `summarize/claude_code.rs:299,383` |
| Model picker = **hardcoded `<select>`**, backing field `provider_model` is a free string (the `ab413c6` regression locus) | shipped | `settings.component.ts:352-359`, `settings/config.rs:69` |
| Note row stores `provider_id` but **no model, no usage**; both `provider_model`+`provider_id` are in scope at save (provenance is free today) | shipped | `storage/models.rs:135-139`, `pipeline.rs:572,590-592` |
| `RedactingProvider` = single cloud chokepoint; already counts redaction tokens | shipped | `summarize/redact.rs:278,319-357` |
| Restore-on-WHOLE-reply (the streaming blocker) | shipped | `summarize/redact.rs:344-358` |
| Agentic loop drives a **gated** executor — model emits `{tool,args}` strings only, never touches the DB | shipped | `agent.rs:18-22,51-64,72-156` |
| Existing FE stream seam: `DeltaSink`→`EVENT_ASSISTANT_TOOL`/`EVENT_CHAT_TOOL`, FE renders chips | shipped | `agent.rs:58`, `transcribe/live.rs:470-500`, `assistant.store.ts:205,367` |
| The two `complete()`+`parse_first_json` JSON side-tasks (structured-output candidates) | shipped | `summarize/timeline.rs:48`, `summarize/graph.rs:31` |
| On-device embedder, **no HTTP embed path**, `EMBED_DIM=384` ("changing it is a schema change") | shipped | `embed.rs:24-39`, `embed/candle_bert.rs:148` |
| Analytics tab: meetings/duration/status/day-chart — **no model/usage panel** | shipped | `features/analytics/analytics.component.ts:46-176`; `storage/db.rs:2035` |
| Preserve-only consent pattern (can't be flipped by a normal save) + dedicated grant commands | shipped (the pattern to copy) | `settings/config.rs:125-136,515-528`; `commands.rs:2209-2214` |
| Generic Keychain secret helpers by account (mirror for a gateway key) | shipped | `secrets/keychain.rs:948,971,992`; `commands.rs:2376-2387` |
| Automatic fallback / token ledger / model catalog / cache | **none** (greenfield) | — |

---

## Findings

### 1. The metadata field-map — only 3 things are standard; the rest is a bag of vendor headers

Three API shapes in play: OpenAI-compat (any gateway), Anthropic-native (our `anthropic` provider already hits this), and vendor extensions. What's portable vs locked decides every feature's reliability.

| Signal | Standard across all OpenAI-compat? | Where | Conf. |
| --- | --- | --- | --- |
| **Token usage** (`usage.{prompt,completion,total}_tokens`) | **STANDARD** (body) — Anthropic uses `input_tokens`/`output_tokens` | response body | high |
| **Model actually served** (post-routing/fallback) | **STANDARD** (body `model` echo) + vendor header | body / header | high (body) |
| **Model catalog** `GET /v1/models` | **STANDARD** (minimal: `{id,object,created,owned_by}`; OpenRouter is rich: ctx-length, pricing, modalities) | endpoint | high |
| Cached-prompt tokens (`usage.prompt_tokens_details.cached_tokens`; Anthropic `cache_read_input_tokens`) | near-standard | body | high |
| Computed **cost** | VENDOR (LiteLLM `x-litellm-response-cost` USD; OpenRouter `usage.cost`) — **and §10-blocked** | header/body | high / med |
| **Cache-hit** flag | VENDOR (`cf-aig-cache-status`, `x-portkey-cache-status`, `X-OpenRouter-Cache-Status`); portable proxy = `cached_tokens>0` | header | med-high |
| **Trace / generation id** (deep-link) | VENDOR (`cf-aig-log-id`, `x-litellm-call-id`, `x-portkey-trace-id`, OpenRouter body `id`) | header/body | high |
| Latency | VENDOR (`X-Kong-Upstream-Latency`, `x-litellm-response-duration-ms`) | header | high |
| Route/fallback fired; guardrail/DLP fired | VENDOR (`cf-aig-step`, `…last-used-option-index`; `cf-aig-dlp`) | header | med-high |

**Design rule that falls out:** anchor every feature on the **universal body fields** (`usage.*` + `model` + `/v1/models`); read trace-id/cost/cache/latency as **progressive enhancement** that silently degrades to `None` on gateways that don't send them. A small per-vendor `GatewayDialect` (Kong|LiteLLM|Portkey|OpenRouter|Cloudflare|Generic) parses the header bag into one `CallMeta`. **Never build on a single vendor's headers** — that re-introduces the lock-in we're avoiding.

### 2. The killer reframe — the best data is already on the wire

`anthropic.rs`'s `MessagesResponse` deserializes only `content`+`stop_reason` and **drops `usage` + `model`** (`anthropic.rs:91-97`). Anthropic returns `usage.{input_tokens,output_tokens,cache_read_input_tokens,...}` + a `model` field on every response. So:
- An authoritative **token-usage receipt** + **which-model-served provenance** needs **zero new gateway** for our existing `anthropic` users — it's a self-contained parsing change.
- The gateway's added value is (a) the same data for `claude_code`-via-gateway / LiteLLM / OpenRouter users, (b) `GET /v1/models` (catalog/picker), (c) a trace-id deep-link. Real, but the *headline metric* (usage + provenance) is already free.

This collapses the perceived "needs a gateway" dependency and makes the cheapest, highest-value spike: **parse `usage`+`model` in `anthropic.rs` and stop throwing them away.**

### 3. The one real architectural fork — return-type vs side-channel

The trait returns bare `String`, so there's nowhere to put usage/model. Two honest options:
- **(a) Widen the return** to `(String, CallMeta)` — clean, per-call, concurrency-safe, but touches ~13 call sites (`pipeline.rs:573`; `commands.rs:1104,1351,1838,1875,1936,2091`; `summarize/{graph,organize,timeline}.rs`; the `RedactingProvider` pass-through).
- **(b) Side-channel** — an `AppState`-held `Mutex<Option<LastCallMeta>>` each provider writes (mirrors how `live_transcript: Mutex<String>` was added in 0.6.2). Additive, zero call-site churn.

**Tension to resolve in the spec phase:** the agentic loop fires several `complete()`s, sometimes concurrently — a shared `Mutex` side-channel races, which *argues for (a)* despite the churn. Recommendation: side-channel for the simple summarize/note path (one call), but prefer the additive richer-method approach (below) for the brain. Don't pick blindly — this is the design decision.

For **new capabilities**, the clean pattern is **additive default methods** on the trait (object-safe, no break; existing providers inherit the one-shot default; only `OpenAiCompatProvider` overrides):
- `complete_json(system,user,schema) -> Result<Value>` — default = today's schema-in-prompt + `parse_first_json` (`reason.rs:511`); override sends `response_format:json_schema`.
- `complete_streaming(system,user,sink) -> Result<String>` — default emits the whole string as one chunk; override consumes SSE.

### 4. Advanced capabilities — what to wire into the brain/pipeline

| Capability | Through OpenAI-compat? | Plug-in (file:line) | Verdict |
| --- | --- | --- | --- |
| **Streaming (SSE)** | Yes (Kong/LiteLLM/OpenRouter) | NEW `complete_streaming`; emit via `DeltaSink`→`EVENT_ASSISTANT_DELTA` (`agent.rs:58`, `live.rs:470`) | **Build (the deferred token-streaming), gateway-first.** Blocker is OUR firewall: restore-on-whole-reply (`redact.rs:355`) — a `⟪EMAIL_1⟫` token can span two SSE chunks → needs a stateful de-tokenizer (~50 lines, buffer until token complete). On Kong, streaming XOR cache/guard. |
| **Structured output (`json_schema` strict)** | Yes (Claude Sonnet 4.5/Opus 4.1+, OpenAI, Gemini via OpenRouter; errors-not-silent) | NEW `complete_json`; used by `timeline.rs:48`, `graph.rs:31` | **Build — cheapest real win.** Removes the recover-from-noise failure mode on the two JSON side-tasks. Restore-on-whole-reply still works (single object). |
| **Native function-calling** | Yes (LiteLLM streaming+tools buggy, #17246) | would replace `agent.rs:99` model-output shape | **Skip.** Our `GatedToolExecutor` is strictly safer (model never reaches the DB); native tool-calls buy ~nothing, lose control + transport-uniformity. |
| **Embeddings via gateway** | Yes (`/v1/embeddings`) | NEW `HttpEmbedder: Embedder` (`embed.rs:39`) | **Defer.** Clean seam, but dim≠384 → `vec0` re-schema + full re-index, and continuous note-chunk egress is a privacy step backward. Opt-in only on user demand; local e5 stays default. |
| **Semantic cache** | Kong: Enterprise + Redis/pgvector; no streaming cache | belongs in our SQLite, not the provider | **Skip for provider.** Single-user low hit-rate; revisit SQLite-native for the Teams tier. |
| **Prompt-guard / moderation** | Yes (but response-phase, off-device) | n/a (sits on gateway, post-egress) | **Skip.** Wrong side of the firewall — our thesis is scrub-before-egress on-device. Defense for a threat we don't have client-side. |

Cross-cutting gateway tradeoff to state loudly: on a real Kong route you get **streaming XOR (cache + response-guard)** — they don't compose. So streaming+structured-output is one coherent lane; cache+moderation is a separate (deferred) lane.

### 5. The full feature catalog — ranked & tiered

Effort: S ≤ ~1 day, M ≈ 2-4 days, L ≈ week+.

| Feature | Surface (file:line) | Data source | Value | Effort | Trait change? | Tier |
| --- | --- | --- | --- | --- | --- | --- |
| **L · Gateway provider + base-URL + Keychain key** (the carrier) | `make_provider` `mod.rs:63`; `config.rs`; `anthropic.rs:8` configurable | n/a (enabler) | High-as-enabler | S | No | **1** |
| **A · Model-catalog picker** from `/v1/models` | `settings.component.ts:352`; new `list_models` cmd | **Std** `/v1/models` | High (structurally kills the `ab413c6` bug) | S | No | **1** |
| **C · Structured gateway error + health dot** | `settings.component.ts` provider block; `anthropic.rs:159` pattern | **Std** error body + `/v1/models` ping | High (attacks claude_code opacity) | S | No | **1** |
| **B · Per-note model provenance** (frontmatter `ai-provider`/`ai-model`) | `pipeline.rs:590`, `template.rs:179`, export | **Std** (`model` echo); requested model free today | High (on-brand owned-files) | S (requested) / M (served) | No (requested) | **1→2** |
| **Security guardrails R1-R4** (treat any URL as cloud; https; key↔URL) | `mod.rs:53,69,125`; new provider | n/a | **Non-negotiable** | rides L | No | **1** |
| **D · Egress & Usage Ledger** (round-1 privacy receipt + token receipts, fused) | new panel `analytics.component.ts:172+`; new `egress_log` table; write at `redact.rs:319` | **Std** `usage.*` + local redaction counts | **High — the wedge** | M | Yes (or side-channel) | **2** |
| **E · Per-task / per-folder model profiles** | `mod.rs:63`; `timeline.rs:48`; `Folder` `models.rs:109` (additive col) | **Std** (model string per task) | High (sensitive folder→local = privacy WIN) | M | No | **2** |
| **G · Token-budget awareness** (NOT currency) | rides D | **Std** `usage.*` | Med | S (on D) | No | **2** |
| **Structured-output hardening** (`complete_json`) | `complete_json`; `timeline.rs`/`graph.rs` | request `response_format` | Med-High | S-M | additive method | **fast-follow** |
| **Token streaming** (`complete_streaming`) | `complete_streaming`; `DeltaSink` | request `stream:true` | High (deferred feature) | M | additive method | **fast-follow** |
| **F · Cache-aware "free regen"** | note/Ask UI | `cached_tokens>0` (std proxy) / vendor hdr | Med | M (on D) | rides D | defer |
| **H · "Open in gateway dashboard"** deep-link | note detail; store trace id | **Vendor** trace id | Med | S | capture id | defer |
| **I · A/B model comparison** for one note | note detail / recipes | 2 calls, different `model` | Med | M-L | No | defer |
| **J · "Best model for your notes" leaderboard** | Analytics | derived B + "kept" flag | Low-Med | M | No | defer |
| **K · Currency $ dashboard** | Analytics | vendor cost | Med | M | Yes | **BLOCKED (§10)** |

### 6. Demand + moat

- **Demand: broad and evidenced, not fringe.** BYO-OpenAI-compatible-endpoint is table-stakes across Obsidian AI plugins (Copilot et al.), and openly requested even against first-party vendors: GitHub Copilot CLI #2283, VSCode #7518, **AnythingLLM #5234 (asks for base URL AND `/v1/models` model discovery — both halves of this brief)**. Meeting-app comparables all ship BYO-LLM; **Meetily gates its custom OpenAI endpoint behind $10/user/mo Pro** (proof it's tier-able toward Teams). (high)
- **Moat: the provider is commodity (zero edge); the Insights layer is uncontested in the privacy space.** The closest comparable, Obsidian Copilot, ships a base-URL field but **no catalog, no token tracking, no ledger, no provenance, no data-destination warning**. "A local meeting app that, over YOUR gateway, shows a private model-catalog + token-usage receipt + model provenance + structured errors" has no direct competitor. The genuine edge is the **usage/privacy ledger** (makes the redaction firewall — our strongest uncontested differentiator, `COMPETITIVE-LANDSCAPE.md:55` — visible and provable) and **structured errors** (auto-explains the claude_code opacity). The catalog is convenience; profiles are a nicety. (high for the comparable read)
- **Audience:** privacy/control power-user now (they already file the issues and already mis-use `ollama_base_url`); cost-router as a bonus (hobbled by §10 → tokens not dollars); **Teams-governed-egress as the deferred long game** (round-1 inversion; keep the `owner_id`-cheap seam clean).

### 7. Security / governance — the risk/mitigation table (the most important section)

A desktop app POSTing redacted content to a user-typed URL has four real risk classes. The **localhost trap is empirically confirmed**: LiteLLM's own docs describe a `127.0.0.1:4000` proxy that routes to "Ollama, OpenAI, Anthropic…" — **localhost is NOT local**.

| # | Risk | Scenario | Real? | Required mitigation |
| --- | --- | --- | --- | --- |
| **R1** | Content egress to arbitrary/hostile/typo'd URL | user types `https://evil.tld/v1`; raw transcript POSTed | high | **Treat ANY gateway URL as cloud egress** → wire into `is_cloud()` (`mod.rs:53`) so it gets `RedactingProvider` + the fail-closed consent gate. The single most important guardrail. |
| **R2** | `localhost` falsely treated as "safe" → skips redaction | localhost LiteLLM forwards to OpenAI's cloud | high (confirmed) | **Never exempt by hostname.** Redaction + consent apply even for `127.0.0.1`. (Contrast: native `ollama` is a *terminal* local model; a generic gateway is opaque.) **Fixing the existing `ollama_base_url` exemption is a prerequisite — or the new provider must not reuse it.** |
| **R3** | API key sent to the wrong endpoint | gateway/OpenAI key sent to whatever URL is typed | high — **documented P0** (Hermes-agent #28660) | **Bind key to URL.** Gateway key in Keychain (own account), sent ONLY to the configured gateway URL; never fall back to another provider's key. |
| **R4** | Plaintext-HTTP downgrade | `http://remote/v1` sends content unencrypted | high | **Enforce `https://` for non-loopback**; allow `http://` only for `127.0.0.1`/`localhost`/`::1`; reject `file://`/`ftp://` (SSRF scheme-allowlist). Reuse the TLS-1.2 floor (`anthropic.rs:83`). |
| **R5** | No visibility into where content went | a typo silently sent 3 meetings to the wrong host | med | **The Insights receipt IS the mitigation** — show destination host + bytes + redaction counts per call. Security feature = product feature. |
| **R6** | `/v1/models` / trace-id disclosure | catalog fetch or trace-id leaks something | **low — it doesn't** | inbound-only fetch, no content in the request; don't log the key; trace-id is a non-PII id. |

All four core guardrails (R1-R4) are unit-testable headless with the existing `EchoProvider`/`CaptureProvider` doubles — **no real Mac needed** for the security core.

---

## Fit with Murmur's constraints

- **Local-first / privacy — strengthened, not strained** (once the guardrails land). Every insight is computed at the `RedactingProvider` chokepoint = a local SQLite write that also records what was scrubbed; no new egress. E's "sensitive folder → local model" is a privacy *gain*. The new provider FIXES the `ollama_base_url` gap by establishing the correct cloud-gated pattern.
- **Obsidian-native / owned files.** Provenance (B) writes into the user's `.md` frontmatter (English-keyed), not a vendor console. The ledger (D) can optionally export a monthly `.md` digest to the vault.
- **SQLite-canonical.** `egress_log` is an additive, guarded table (`add_column_if_missing` / `CREATE TABLE IF NOT EXISTS`) — never a gateway's Redis/pgvector. Usage *originates* at the gateway but is *owned* in our store.
- **Provider seam + redaction intact.** New capabilities are additive default methods (object-safe, no break). The return-type fork (§3) is the one design decision to make deliberately.
- **No-PII-in-logs (§8).** The ledger stores host + model id + token **counts** + PII-token **counts by kind** — never scrubbed values/prompt/transcript.
- **No-currency (§10).** Ledger stays **token-denominated**. Dollar cost (data exists: LiteLLM/OpenRouter) is an explicit, user-approved exception only — do not introduce amount fields by default.
- **macOS / CI honesty.** All Tier-1/Tier-2 features + the security core are `cargo test --lib` + Playwright-against-`:1420` testable. The only "needs a real Mac + a live gateway" parts: end-to-end that the `claude` CLI honors `ANTHROPIC_BASE_URL`, that a real LiteLLM echoes `model`, and the streaming SSE chunk-split behavior.

---

## Options & tradeoffs

- **Option 1 — "Gateway Insights Lite" (L + A + B-requested + C + guardrails R1-R4).** Effort **S** total, risk **low**, no trait change, no new table. Ships the safely-gated provider, live model picker, provenance-in-frontmatter, structured errors+health. Closes the `ollama_base_url` gap by giving power-users a correct path. **The safe, high-ROI first PR.**
- **Option 2 — full "Gateway Insights" (add D + E + B-served + G).** Effort **M** on top, risk low-med (the usage side-channel is the only subtle bit). Ships the Egress & Usage Ledger (the differentiator-maker) + per-task/per-folder profiles + token-budget awareness. **The spec's true target.**
- **Capability fast-follow — `complete_json` then `complete_streaming`.** Effort S-M then M. Hardens the JSON side-tasks; delivers the long-deferred token streaming (gateway-first; first RED test = a scrub-token split across two SSE chunks).
- **Option 3 — delight (F + H + I + J).** Effort L, diminishing single-user returns. Defer.
- **Excluded: K (currency, §10).**

---

## Recommendation & first step

**Build Option 1, then Option 2; fast-follow `complete_json` + streaming; exclude K; defer Option 3. Independently, send the `ollama_base_url` exemption to `lock-security-reviewer` now — it's a live shipped egress gap, not contingent on this feature.**

**Smallest verifiable first slice (an afternoon, no Mac):** the highest-value, lowest-risk spike is surprise #1 — **parse `usage` + `model` in `anthropic.rs`'s `MessagesResponse` and stop discarding them**, returned via the chosen side-channel, asserted against a recorded fixture (token counts + served-model surface correctly). This proves the headline data was always on the wire, validates the side-channel choice before any UI, and is independently shippable. In the same PR or right after, the **security spike**: add the `gateway` provider id and assert RED-before-GREEN the four invariants — (1) `is_cloud()` → built only with consent (reuse `cloud_providers_refused_without_consent`, `mod.rs:207`); (2) `RedactingProvider`-wrapped even when base URL is `http://127.0.0.1` (the anti-localhost test); (3) non-loopback `http://` rejected at construction; (4) the gateway key is sent only to the configured URL. That single spike de-risks the security thesis AND fixes the latent `ollama_base_url` pattern.

**Build order:** L+A (model picker, kills the `ab413c6` class) → C (structured errors+health) → B-requested (frontmatter provenance, free today) → the usage side-channel (parse `anthropic` `usage`+`model`; wire `claude -p --output-format json`) → D (`egress_log` + fused Analytics panel; B-served falls out) → E (additive `Folder.model` + per-task model strings) → fast-follow `complete_json` → `complete_streaming` (+ the firewall de-tokenizer).

---

## Open questions / what couldn't be verified

- **Return-type fork (side-channel vs trait widening)** — recommended but not prototyped against all ~13 call sites; the agentic loop's concurrent `complete()`s may race a shared `Mutex` side-channel → may force per-call return (widening) for the brain. Decide in the spec's design phase.
- **Does the `claude` CLI honor `ANTHROPIC_BASE_URL`, and does `claude -p --output-format json` reliably carry `usage`/model?** Passed through under `claude_code_inherit_env` (`config.rs:201`) but not end-to-end tested vs a running LiteLLM/Kong — needs a real Mac + a gateway.
- **Whether gateways echo the *served* `model` in the body vs only a header** — read both (body first, header fallback), degrade if absent. Not exhaustively verified across Kong/Portkey/vLLM.
- **Whether `ollama`'s non-cloud exemption was *intended* to cover remote `ollama_base_url`** — code says it does today; whether deliberate ("Ollama is trusted-local") or oversight is a maintainer/`lock-security-reviewer` call. Changes whether the gateway is a new id or a tightening of the existing one.
- **Streaming firewall de-tokenizer correctness under partial multi-byte UTF-8 split** (`⟪⟫` are 3-byte chars) across SSE chunks — feasible, the test matrix is the risk; reasoned from `redact.rs:355`, not prototyped.
- **Vendor field names/headers are point-in-time June 2026** (OpenAI `usage`/`cached_tokens`, OpenRouter `usage.cost`/`X-Generation-Id`, LiteLLM `x-litellm-*`, Anthropic `cache_read_input_tokens`, Portkey/Cloudflare/Kong headers) — re-verify before coding. Portkey headers came via search of official docs (clean primary fetch blocked by a redirect); Kong's `X-Kong-LLM-Model`/`X-Cache-Status` from the cost cookbook, not the ai-proxy reference page.
- **Demand intensity (upvote counts)** — GitHub's API hid reaction counts; breadth is confirmed, raw ranking is not. Receipt-drives-retention is a reasoned inference (Murmur ships no telemetry), not measured.

---

## Sources

**Code (this repo):**
- `src-tauri/src/summarize/anthropic.rs:8,91-97,159-168,83` — hardcoded URL; **`MessagesResponse` discards `usage`+`model`** (surprise #1); error-extraction pattern; TLS-1.2 floor.
- `src-tauri/src/summarize/mod.rs:53,54,63,69,102-107,125,207` — `is_cloud`; **ollama returns before the redaction wrap** (surprise #2 / the gap); `make_provider`; consent gate; the consent-refusal test.
- `src-tauri/src/summarize/provider.rs:50,54` — bare `Result<String>` (the metadata-channel constraint / return-type fork).
- `src-tauri/src/summarize/redact.rs:278,319-358,409+` — chokepoint; redaction-token counts; restore-on-whole-reply (streaming blocker); mock-provider doubles.
- `src-tauri/src/summarize/{timeline.rs:48,graph.rs:31}` — the two `parse_first_json` side-tasks (structured-output beneficiaries).
- `src-tauri/src/agent.rs:18-22,51-64,72-156` — gated executor + the existing `DeltaSink` stream seam.
- `src-tauri/src/reason.rs:417,425-469,511-522` — `CloudReasoner` (no HTTP client) + `structured()` (the `complete_json` default blueprint).
- `src-tauri/src/embed.rs:24-39` — `Embedder` seam + `EMBED_DIM=384` schema-change catch.
- `src-tauri/src/settings/config.rs:69,77,125-136,191-201,515-528` — free-string model field; `ollama_base_url`; preserve-only consent; `claude_code_inherit_env`; grant commands.
- `src-tauri/src/storage/models.rs:109,135-139`; `pipeline.rs:572,590-592`; `storage/db.rs:2035-2121` — `Folder` (no policy col); `NoteRecord` (provider_id, no model/usage); provenance free at save; analytics query (no usage aggregation).
- `src/app/features/{analytics/analytics.component.ts:46-176, settings/settings.component.ts:352-359}` — Analytics surface; hardcoded model `<select>`.
- `secrets/keychain.rs:948,971,992`; `commands.rs:2209-2214,2376-2387` — Keychain helpers + consent/key command patterns.
- `docs/COMPETITIVE-LANDSCAPE.md:55`; `docs/ARCHITECTURE-LOCAL-CLOUD.md` Feature 5; `docs/research/2026-06-30-kong-ai-gateway-fit.md` — differentiator framing; deferred Teams seam; round 1.

**External (point-in-time June 2026; URLs fetched unless noted):**
1. https://docs.litellm.ai/docs/proxy/response_headers — `x-litellm-response-cost` (USD), `x-litellm-model-id`/`-model-group`, `x-litellm-call-id`, `x-litellm-response-duration-ms`.
2. https://docs.litellm.ai/docs/proxy/caching — `x-litellm-cache-key` on cache hits.
3. https://docs.litellm.ai/docs/simple_proxy — localhost proxy routes to Ollama/OpenAI/Anthropic (localhost ≠ local; R2).
4. https://openrouter.ai/docs/cookbook/administration/usage-accounting + /guides/features/response-caching — `usage.cost`/`cost_details`; always-on; last-SSE-chunk; `X-OpenRouter-Cache-Status`/`-Age`/`-TTL`; usage-zeroing on hit.
5. https://openrouter.ai/docs/guides/features/structured-outputs + /api/reference/streaming — `json_schema` strict (Claude 4.5/Opus 4.1+, errors-not-silent, works with streaming); SSE.
6. https://openrouter.ai/docs/api/api-reference/models/get-models — rich `/api/v1/models` (context_length, pricing, architecture, supported_parameters).
7. https://developer.konghq.com/ai-gateway/streaming/ + /plugins/ai-semantic-cache/ + /cookbooks/llm-cost-optimization/ — SSE token-by-token disables response-phase plugins; no streaming cache; `X-Kong-LLM-Model`/latency/$-budget headers.
8. https://developers.cloudflare.com/ai-gateway/glossary/ + /features/caching/ — `cf-aig-cache-status`/`cf-aig-log-id`/`cf-aig-event-id`/`cf-aig-step`/`cf-aig-dlp`.
9. https://docs.portkey.ai/docs/api-reference/response-schema (via search) — `x-portkey-trace-id`/`x-portkey-cache-status` (HIT/SEMANTIC HIT/MISS/…)/`x-portkey-retry-count`/`x-portkey-last-used-option-index`.
10. https://platform.openai.com/docs/api-reference/models/list + /guides/prompt-caching — standard `/v1/models`; `usage` + `prompt_tokens_details.cached_tokens`; `stream_options:{include_usage:true}`.
11. https://github.com/github/copilot-cli/issues/2283 + https://github.com/Mintplex-Labs/anything-llm/issues/5234 — BYO base-URL demand; AnythingLLM asks base URL + `/v1/models` discovery.
12. https://github.com/NousResearch/hermes-agent/issues/28660 — P0 cross-provider credential leak (R3).
13. https://portswigger.net/web-security/ssrf + https://developer.mozilla.org/en-US/docs/Web/Security/Attacks/SSRF — SSRF scheme-allowlisting (R1/R4).
14. https://meetily.ai/docs/integrations/custom-apis/ — meeting-app comparable; custom OpenAI endpoint gated behind $10/user/mo Pro (Teams/monetization signal).
15. https://www.obsidiancopilot.com/en/docs/settings — closest comparable ships base-URL but no catalog/token-tracking/ledger/provenance/destination-warning (the Insights gap).
16. https://github.com/BerriAI/litellm/issues/17246 — streaming+tool_calls drop (keep our gated executor).
17. https://openai.com/index/introducing-structured-outputs-in-the-api/ — schema-adherence vs JSON-mode.
18. https://blog.cloudflare.com/block-unsafe-llm-prompts-with-firewall-for-ai/ — gateway moderation runs post-egress (wrong side of our firewall).
