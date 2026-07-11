<!-- Generated 2026-07-10 via /research (murmur-researcher fan-out, 5 angles: compat / plugin-licensing / deployment-topology / team-tier / MCP-A2A). Kong pricing/licensing/version facts are point-in-time (July 2026, Kong AI Gateway ~3.10–3.14) — re-verify against Kong's OSS `constants.lua` + plugin `tier:` badges before quoting later. Consolidates + operationalizes the two prior Kong docs (2026-06-30-kong-ai-gateway-fit.md, 2026-07-02-auto-model-routing-vs-clickup-kong.md) now that the `gateway` seam is fully built. -->
# Research: How Murmur can MAXIMALLY leverage Kong AI Gateway

## TL;DR / Verdict

**"Maximally leverage Kong" ≠ "adopt as much Kong as possible." The maximal *value* is a thin slice that is ~90% already built, because everything heavy in Kong is Enterprise/Konnect-gated and collides with our local-first + no-per-seat-fee moat.** Three concrete moves, in ladder order:

1. **NOW (S, ~0 code):** we already ship a fully-hardened, Kong-branded `gateway` provider (OpenAI-compatible, cloud-classified even on loopback, redaction-wrapped, consent-gated, egress-ledgered, dedicated Keychain key). Kong's **free/OSS `ai-proxy`** plugin is hit **zero-code** by that provider. Maximal use today = **validate it with one Docker spike + write a power-user "point Murmur at your own Kong" recipe + one privacy-receipt clarification.** Nothing to build.
2. **CHEAP WIN (S, native — NOT Kong):** the only governance Kong offers that we lack is **broader PII category coverage** (IBAN, passport, IP, national-ID…). Harvest Kong's public PII taxonomy and extend `redact.rs` natively — no enterprise license, no daemon, in-process. Gate on a grep of real transcripts first (may be unnecessary).
3. **LATER / Phase-3 bet (defer):** a shared AI gateway in front of a future **hosted/team-tier** LLM-egress arm unlocks central key vault + per-seat token budgets + **usage-metering→Stripe/ERP billing** — a real monetization engine. But every monetization-grade Kong feature is **Konnect-SaaS/Enterprise**, and it's **not uniquely Kong** (LiteLLM/Portkey fill the same slot; we already own the local metering signal in `egress_log`). Hold the option open via the existing seam at zero cost; decide when the tier is real.

**Never:** bundle a Kong daemon into the notarized `.app`; route the desktop app through Konnect (Kong Inc. as a 3rd data-path party — rejected on the *provable* local-first narrative even though telemetry is documented content-free); put Kong in front of `murmur-server` (zero-knowledge, no AI traffic, no plaintext — disjoint by design); or adopt Kong MCP/A2A gateways for our single-user loopback MCP.

This report **consolidates and operationalizes** the two prior Kong docs (both said "don't bundle, steal ideas, shelf for Teams"); the new facts are (a) the `gateway` seam is now fully built + Kong-labeled, so step 1 is essentially free, and (b) the concrete zero-code config, PII-harvest list, and team-tier/metering + MCP/A2A dispositions.

---

## What we already have (grounded in the tree)

The single most important fact: **the seam Kong would occupy is already built and shipping, and the UI already calls it "Kong AI Gateway."**

- **`OpenAiCompatProvider`** — `src-tauri/src/summarize/gateway.rs` — hits any OpenAI-compatible `/v1/chat/completions` (`chat_body` :67; `chat_body_json` with `response_format: json_schema` :81) + `/v1/models` (:272). `resolve_chat_endpoint` (:250) keeps a **custom path (a Kong route) verbatim** (test `resolve_chat_endpoint_custom_path_as_is` :1021). URL guardrails `validate_gateway_url` (:43): https-only except loopback, no embedded creds, no SSRF schemes. Bearer key from Keychain (`gateway_api_key`), **never** falls back to the Anthropic key (invariant R3, :7).
- **Always cloud + always redacted + consent-gated** — `src-tauri/src/summarize/mod.rs`: `egress_is_cloud("gateway")` is always true even on loopback ("a localhost gateway can still FORWARD to the cloud", :66-72); `make_provider_resolved` refuses to construct it until `cloud_egress_consented` (:171) and wraps it in `RedactingProvider` (:304). So a Kong endpoint is PII-scrubbed before egress with **zero new firewall work**.
- **Content-free egress ledger** — `src-tauri/src/summarize/egress_log.rs`: `EgressEntry` (:23) carries only ids/host-label/model/token-counts/PII-counts/byte-sizes; `provider_id="gateway"`, destination = URL host only (e.g. `127.0.0.1:4000`, `mod.rs:290`). Security test asserts no content leaks (:251). **This is the local, content-free version of the "token metering" Kong sells.**
- **Kong-branded in the UI** — `roles.rs:62` maps `"gateway" => "Kong AI Gateway"`; also in `ai-connection-cards.component.ts`, onboarding, privacy section. Config `gateway_base_url`/`gateway_model` (`settings/config.rs`, default empty), whose comment lists "**LiteLLM / Kong / Portkey / vLLM**".
- **Redaction firewall** — `src-tauri/src/summarize/redact.rs`: regex (email/card/phone) + on-device NER name masking (mDeBERTa `Davlan/mdeberta-v3-base-ner-hrl`) with round-trip restore; scrubs **every** `SummarizeRequest` field before egress (coverage test :1896).
- **`murmur-server` is a separate, zero-knowledge world** — AES-encrypted client-side, key in the URL `#fragment` (never sent), server stores only ciphertext; **separate** `share_egress_log` ledger (host + byte-count only, `db.rs:695`). No AI traffic, no plaintext.
- **Local read-only MCP** — `src-tauri/src/mcp.rs`: `127.0.0.1:8765`, NO-egress, bearer auth (fail-closed), DNS-rebinding host/origin allow-lists, body cap, per-read `visibility_clause` gating. Already has, in-process, the controls a gateway sells.
- **Prior art:** `docs/research/2026-06-30-kong-ai-gateway-fit.md` ("don't bundle; steal 2-3 ideas; shelf for Teams") and `docs/research/2026-07-02-auto-model-routing-vs-clickup-kong.md` ("don't build a hidden best-model router"). This report supersedes/operationalizes both.

---

## Findings (per angle; claim → source/confidence)

### A. Kong `ai-proxy` ↔ our `gateway` provider — zero-code happy path
- Kong's OSS `ai-proxy` with `route_type: llm/v1/chat` and default `llm_format: openai` accepts an OpenAI `/v1/chat/completions` body + `Authorization: Bearer` and transforms to/from the Anthropic upstream — **our provider hits it with no code change** (developer.konghq.com/plugins/ai-proxy/; ai-providers/anthropic/ — **high**).
- **Auth matches R3 exactly:** the client's `Authorization: Bearer` is a *Kong consumer key*; Kong holds the real Anthropic `x-api-key` server-side in `config.auth` (**high**).
- **Two real gaps, neither a blocker:**
  - Kong routes live at **arbitrary paths** (`/anything`), so the user must set `gateway_base_url` to the **full route URL** so our custom-path branch fires (or name the Kong route `/v1/chat/completions`) (**high**).
  - `/v1/models` is not documented on Kong → our code returns an empty catalog gracefully; the FE falls back to the manually-typed model (cosmetic degrade, **med**).
  - **`response_format: json_schema` passthrough is the one load-bearing unknown** — Kong's OpenAI→Anthropic transform's field whitelist is undocumented. If stripped, structured-output calls (`complete_json_with_meta`) lose server-side constrained decoding (may still parse free-text JSON). Also **Anthropic requires `max_tokens`** on `/v1/messages` — set it explicitly in Kong `config.model.options`. **Needs one live Docker spike** (**med**).
- **OSS vs Enterprise is clean:** `ai-proxy` is Apache-2.0 (Kong Gateway ≥ 3.6, DB-less supported). `ai-proxy-advanced` (multi-provider load-balancing, retry/fallback, semantic routing) is **Enterprise/Konnect** and needs Redis/pgvector for semantic routing (**high**).

### B. Which Kong AI plugins are actually free — and the only real governance delta
Authoritative OSS test = presence in Kong's open-source bundle (`kong/constants.lua`).

| Plugin | Tier | Offline self-hosted? | Value to Murmur |
|---|---|---|---|
| `ai-proxy` | **OSS** | yes | the transport we already use — no new value, but the zero-code target |
| `ai-prompt-guard` (regex allow/deny) | **OSS** | yes | low (single-user doesn't need prompt allow-lists) |
| `ai-prompt-decorator` / `ai-prompt-template` | **OSS** | yes | none (we template on-device) |
| `ai-request/response-transformer` | OSS-listed but **calls an external LLM** to work | no | none (adds an LLM round-trip) |
| `ai-proxy-advanced` (LB/fallback/semantic-routing) | **Enterprise** | — | none for n=1; and our CLI+HTTP+local seam already fails over *better* than an HTTP-only gateway |
| `ai-semantic-cache` | **Enterprise** | needs Redis/pgvector + embeddings LLM | **~0** — meeting prompts are near-unique, hit-rate ≈ 0 |
| `ai-rag-injector` | **Enterprise** | needs vector DB + external embeddings | none — we do RAG on-device |
| `ai-prompt-compressor` | **Enterprise** | separate Docker service | low |
| **`ai-sanitizer` (AI PII Sanitizer)** | **Enterprise** | **yes** — local Docker anonymizer (`localhost:8080`), 18 categories / 9 langs, no cloud call for redaction | **the only interesting one — but enterprise-licensed AND ~duplicates `redact.rs`**; the sole real delta is *category breadth* |
| `ai-azure-content-safety` / `ai-aws-guardrails` / `ai-lakera-guard` | **Enterprise** | **no** — call external cloud APIs | none / actively bad (new egress + cost) |

- **The pattern:** the free plugins are precisely what we already own in-process; every valuable governance plugin is Enterprise/Konnect (developer.konghq.com plugin `tier:` badges; `constants.lua` — **high**).
- **AI PII Sanitizer as "layer 2" is not a realistic win:** enterprise-licensed, needs a Docker sidecar per user, and its *name-NER* overlaps our mDeBERTa. The **only** delta is structured-PII breadth (IBAN/passport/IP/national-ID/medical/crypto-address). **Recommendation: harvest the taxonomy, extend `redact.rs` natively** (**high**).

### C. Deployment topology for a DESKTOP app (no server) + the privacy tension
- **Kong AI Gateway is a server (OpenResty/Lua), Docker-oriented, DB-less ~2 GB RAM reserved** (discuss.konghq.com DB-less memory thread; DB-less docs — **high**). Riding on Docker Desktop's multi-GB VM, this **collides with Murmur's OOM history** (`perf-oom-open-meeting-2026-07-07`). **Do not bundle / auto-launch.**
- **Data-control by topology:**
  - **(a) per-user loopback Docker** — no new data controller (Kong on `127.0.0.1`, DB-less OSS, no control plane). Our seam already handles loopback. **Recommended, opt-in, power-user.**
  - **(b) org self-hosts one shared Kong** — device boundary preserved, but the org operator now sees every member's *redacted* transcript in transit (a new internal trust boundary; fine for an org that owns the box, must be stated).
  - **(c) Konnect (Kong-hosted control plane)** — inserts Kong Inc. into the trust story. Kong's CP↔DP telemetry is **documented content-free** ("does not include any customer information or any data processed by the Data Plane", developer.konghq.com/gateway/cp-dp-communication/ — **high**), and payload capture is opt-in Debugger only. **But** "trust Kong's word the telemetry is clean" is a strictly weaker story than "bytes never left a process you launched." For a *provable* local-first brand — **reject (c) for the desktop product.**
- **Privacy framing (important):** loopback-Kong→Anthropic sends the **same redacted bytes** to Anthropic as the direct `anthropic` provider (the firewall runs *before* the provider in both). So it is **privacy-neutral — an operational upgrade (audit/budgets/failover), not a privacy upgrade.** Sell it as such.
- **Lighter alternative:** for "a local forwarder with budgets/observability," **LiteLLM proxy** (Python, no mandatory Docker, far lighter than Kong OSS) is likely the better loopback recommendation on a RAM-constrained Mac — and our `gateway` seam already lists it (**med-high**).

### D. Team/hosted tier + observability/metering; and Kong ≠ murmur-server
- **Kong ≠ murmur-server — unambiguously disjoint (repo already models it as two worlds):** the AI-egress path (redacted prompt leaves → LLM/gateway; `egress_log`) vs the sharing path (`murmur-server`, zero-knowledge, ciphertext-only, `share_egress_log`, host+bytes only). An AI gateway **must** see the (redacted) prompt to proxy/meter it; the sharing relay **never** sees plaintext. **Kong can only sit in front of a future shared LLM-egress arm — never in front of the sharing relay** (would break the zero-knowledge moat) (**high**).
- **For a hosted/team tier Kong's checklist is strong but Konnect-gated:** centralized key vault, per-seat/per-tier **token-aware** rate-limiting (`ai-rate-limiting-advanced` = Enterprise), cost control, semantic cache, **GenAI OTLP** spans/metrics (base `opentelemetry` plugin OSS; GenAI attributes are AI-Gateway-version-gated), and **usage-metering → Stripe/ERP billing** (developer.konghq.com/metering-and-billing/ — **requires a Konnect account + `spat_` Ingest token; no self-hosted OSS path**, **high**). The metering→billing engine could literally drive usage-based monetization — but adopting it = adopting a US-SaaS control plane for billing metadata (token counts + customer ids egress to Konnect).
- **We already own the local metering signal:** `egress_log` counts input/output tokens per call, content-free. The seam and the signal both exist; only the org-scale multiplier is missing.
- **Not uniquely Kong:** LiteLLM/Portkey/Bedrock/Azure fill the same hosted-arm role. Durable framing: *"an AI gateway for the team tier, plausibly Kong — Kong wins iff the customer org already runs Kong."*

### E. Kong MCP gateway + A2A gateway vs our local MCP — skip
- Both are **enterprise fleet-governance control planes** for orgs running *many* networked MCP servers / multi-agent A2A traffic. Kong's own guidance: *"add a gateway when complexity emerges: more than 2-3 servers"*; a *"single local MCP server would NOT benefit"* (konghq.com learning-center — **high**). Enterprise-only, paid plugins.
- Murmur is the opposite: **one single-user, loopback, read-only MCP** (`mcp.rs`) + **one in-process agentic loop** (`agent.rs`, no A2A traffic at all). Neither Kong product has a job here now.
- **Only conditional future fit:** a *cloud-exposed* org MCP endpoint — which our own `docs/research/2026-07-10-shared-brain-org-context.md` **explicitly rejects** (keeps MCP local via an `org_search` tool; server stays zero-knowledge). So the trigger is unlikely. **Skip; re-open only as a build-vs-buy if that rejected path is ever taken.**

---

## Fit with Murmur's constraints

- **Local-first / privacy:** the `gateway` seam is *already* cloud-classified + redaction-wrapped + consent-gated + ledgered even on loopback (the clean part — no new egress bypass). The *strains* are all in the heavy Kong: Konnect (3rd-party trust), semantic-cache/PII/guardrails (external services or Docker sidecars), and Konnect metering (US-SaaS billing metadata). Keep the thin slice, reject the heavy machinery.
- **Obsidian-native / SQLite-canonical:** untouched — Kong is pure outbound LLM transport. Metering/cache must stay in *our* SQLite (`egress_log`), not a gateway's Redis (else a divergent copy of truth).
- **Provider seam + redaction firewall:** any gateway *rides* the existing trait with zero new code, always `RedactingProvider`-wrapped. No violation.
- **macOS-first / CI honesty:** *pointing at* an external gateway is free + headless-testable (the `gateway` provider has ~30 unit tests). *Bundling* a daemon is the burden (sign/notarize/supervise a container) — out of scope. The json_schema/max_tokens/models behaviors are live-HTTP → need a real Docker spike, not `cargo test`.
- **No new deps / no per-seat moat:** the OSS slice adds nothing to compile. Every Enterprise plugin needs a paid Kong license — antithetical to the no-per-seat-fee positioning until a hosted tier exists.

---

## Options & tradeoffs

| Option | Effort | Risk | Unlocks |
|---|---|---|---|
| **1. Validate + document the existing `gateway`→Kong path** (Docker spike settles json_schema/max_tokens/models; power-user recipe doc; privacy-receipt "loopback gateway = forwarder" note) | **S** | ~0 | the supported "BYO Kong/LiteLLM" path; positions Murmur gateway-agnostic without owning Kong's lifecycle. **← do now** |
| **2. Harvest Kong's PII taxonomy → extend `redact.rs`** (IBAN/IP/passport/national-ID validators + round-trip tests; gate on a transcript grep first) | **S** | low | broader PII coverage natively — the only real governance delta, without license/daemon. **← cheap win** |
| **3. Shared AI gateway in front of a future hosted/team LLM-arm** (Kong Konnect *or* LiteLLM/Portkey: key vault, per-seat token budgets, metering→billing) | **L** | high (SaaS dependency; premature at n=1) | team-tier monetization. **← defer to Phase-3; hold option open via the existing seam at zero cost** |
| **4. Bundle a Kong daemon / adopt Konnect for the desktop app / Kong MCP-A2A / Kong-in-front-of-murmur-server** | L | very high | nothing we need; breaks local-first / zero-knowledge / no-per-seat moat. **← reject, on the record** |

---

## Recommendation & first step

**Do 1 now, 2 as a cheap follow-up, defer 3, reject 4.** "Maximal leverage" here is deliberately a thin slice: we already shipped the valuable 90% (the hardened, redaction-wrapped, Kong-labeled `gateway` seam); Kong's heavy machinery is enterprise-gated and would cost the moat, not strengthen it.

**Smallest verifiable first slice (½ day, one throwaway Docker):**
1. Run OSS Kong ≥ 3.6 DB-less with the declarative config below (one `ai-proxy` route → Anthropic).
2. Point Murmur's `gateway_base_url` at `http://127.0.0.1:8000/v1/chat/completions`, `gateway_model = claude-…`, Kong consumer key in `gateway_api_key`.
3. Assert: (a) a note generates (proves zero-code happy path); (b) does `response_format: json_schema` survive to Anthropic and does a body **without** `max_tokens` succeed (settles the two Angle-A unknowns → turns every `med` here to `high`); (c) `/v1/models` 404s (confirms graceful degrade); (d) the `egress_log` row shows `127.0.0.1:8000` + correct redaction count, content-free; (e) the redaction firewall still scrubbed before the request left.
4. If (b) shows json_schema is stripped, add the small `complete_json_with_meta` fallback (prompt-instructed JSON — we already parse `choices[0].message.content`).

Then ship the power-user recipe doc + the one-line privacy-receipt clarification. **Consider recommending LiteLLM proxy as the lighter default loopback target** on RAM-constrained Macs (same seam, no 2 GB Kong).

### Durable one-line decisions to record
1. Kong lives in front of the **future shared LLM-egress arm ONLY** — never in front of `murmur-server` (zero-knowledge, no AI traffic, no plaintext). Two rozłączne światy.
2. Every **monetization-grade** Kong feature (key vault, token-aware rate-limit, metering→Stripe/ERP billing, semantic cache) is **Konnect/Enterprise SaaS** — adopting = a US-SaaS control-plane dependency; weigh against the EU-metal/zero-knowledge posture.
3. Murmur already owns the **local, content-free metering signal** (`egress_log`), the OSS-plugin equivalents in-process, and a `gateway` seam that hits OSS `ai-proxy` zero-code. The only thing missing is org scale — so this is *later*, not *now*.
4. **Do not bundle a Kong daemon; reject Konnect for the desktop; skip Kong MCP/A2A** for the single-user loopback MCP.

### Minimal Kong→Anthropic config (OpenAI-format, decK/DB-less)
Omit `llm_format` (defaults to `openai`). Do **not** copy Kong's `claude-code-anthropic` example (it uses `llm_format: anthropic` native, which our OpenAI-shaped body would NOT match).
```yaml
_format_version: "3.0"
services:
  - name: anthropic-svc
    url: http://localhost:32000            # dummy upstream; ai-proxy overrides routing
    routes:
      - name: openai-chat
        paths: ["/v1/chat/completions"]     # so Murmur's resolver keeps it as-is
plugins:
  - name: ai-proxy
    config:
      route_type: llm/v1/chat
      # llm_format omitted -> "openai": client speaks OpenAI, Kong -> Anthropic
      auth:
        header_name: x-api-key                        # Kong -> Anthropic (real key)
        header_value: ${{ env "DECK_ANTHROPIC_API_KEY" }}
      model:
        provider: anthropic
        name: claude-sonnet-4-5-20250929
        options:
          anthropic_version: '2023-06-01'
          max_tokens: 1024                            # Anthropic requires it; set explicitly
```
Add a `key-auth` consumer so the client's `Authorization: Bearer` is validated by Kong (never the Anthropic key — satisfies R3).

---

## Open questions / what I couldn't verify
- **`response_format: json_schema` passthrough + default `max_tokens`** on Kong's OpenAI→Anthropic transform — undocumented; the one load-bearing unknown for structured output. Settle in the spike above. (med)
- **`/v1/models` on Kong** — no docs either way; assumed absent → our code already degrades. (med)
- **Kong's "telemetry is content-free" (Konnect)** — verbatim from Kong docs, not independently audited by us; that non-auditability is exactly why (c) is a weaker story than a process the user launched. (high on documented, low on provable)
- **GenAI-OTLP tier line** — base `opentelemetry` OSS; GenAI-specific attributes appear AI-Gateway-version-gated (≥ v3.13). (med)
- **Konnect metering for a self-hosted OSS data plane** — get-started requires a Konnect account + `spat_` Ingest token, strongly implying Konnect-SaaS is mandatory regardless of where the gateway runs. (med)
- **Kong AI PII Sanitizer model quality vs our mDeBERTa NER** — no published benchmark found; only the *category breadth* delta is certain. (med)
- **Feature-tier boundaries are point-in-time (mid-2026)** — Kong reshuffles OSS↔Enterprise between minor releases; re-verify against `constants.lua` + plugin `tier:` badges at adoption time.
- **LiteLLM-as-lighter-loopback** not benchmarked here — proposed as a follow-up alternative to Kong OSS on RAM-constrained Macs.

---

## Sources

**Kong / external (point-in-time July 2026):**
- Kong AI Gateway overview / providers / AI Proxy: https://developer.konghq.com/ai-gateway/ · https://developer.konghq.com/ai-gateway/ai-providers/anthropic/ · https://developer.konghq.com/plugins/ai-proxy/ · https://developer.konghq.com/plugins/ai-proxy/reference/ · https://developer.konghq.com/how-to/set-up-ai-proxy-advanced-with-openai/
- OSS vs Enterprise: https://raw.githubusercontent.com/Kong/kong/master/kong/constants.lua (authoritative bundled-plugin manifest) · https://konghq.com/blog/product-releases/announcing-kong-ai-gateway · https://developer.konghq.com/plugins/ai-proxy-advanced/ · https://developer.konghq.com/plugins/ai-semantic-cache/ · https://developer.konghq.com/plugins/ai-rag-injector/ · https://developer.konghq.com/plugins/ai-sanitizer/ (+ /reference/) · https://developer.konghq.com/plugins/ai-prompt-compressor/ · https://developer.konghq.com/plugins/ai-azure-content-safety/ · https://developer.konghq.com/plugins/ai-rate-limiting-advanced/ · https://konghq.com/blog/product-releases/ai-gateway-3-10
- Deployment / privacy: https://developer.konghq.com/gateway/db-less-mode/ · https://discuss.konghq.com/t/kong-gateways-memory-behavior-with-docker-dbless-mode/8703 · https://developer.konghq.com/gateway/cp-dp-communication/ · https://developer.konghq.com/observability/debugger/ · https://www.truefoundry.com/blog/kong-gateway-pricing-architecture-an-analysis-for-ai-teams-2026-edition
- Metering / observability: https://developer.konghq.com/metering-and-billing/ (+ /get-started/, /metering/) · https://konghq.com/blog/product-releases/metering-and-billing-kong-konnect · https://developer.konghq.com/ai-gateway/llm-open-telemetry/ · https://developer.konghq.com/plugins/opentelemetry/
- MCP / A2A: https://konghq.com/blog/product-releases/enterprise-mcp-gateway · https://konghq.com/blog/learning-center/what-is-a-mcp-gateway · https://developer.konghq.com/ai-gateway/a2a/ · https://konghq.com/solutions/agent-gateway

**Code (this repo):**
- `src-tauri/src/summarize/gateway.rs` — `OpenAiCompatProvider`, `chat_body`:67, `chat_body_json`:81, `parse_chat_response`:150, `resolve_chat_endpoint`:250, `resolve_models_endpoint`:272, `validate_gateway_url`:43, R3:7.
- `src-tauri/src/summarize/mod.rs` — `PROVIDER_GATEWAY`, `egress_is_cloud` always-cloud gateway:66-72, consent gate:171, `RedactingProvider` wrap:304, destination-host label:290.
- `src-tauri/src/summarize/egress_log.rs` — content-free ledger :23, no-content test :251.
- `src-tauri/src/summarize/redact.rs` — `RedactingProvider`:540, regex :177-191, `active_name_redactor`:87, mDeBERTa :56, field-coverage test :1896.
- `src-tauri/src/summarize/roles.rs:62` — `"gateway" => "Kong AI Gateway"`.
- `src-tauri/src/storage/db.rs` — `egress_log`:620 vs `share_egress_log`:695 (host+bytes only), `insert_share_egress`:1524.
- `src-tauri/src/mcp.rs` — loopback-only, NO-egress, bearer auth, host/origin/body hardening, per-read visibility gating.
- `src-tauri/src/agent.rs` — single in-process `run_agentic_loop` + `GatedToolExecutor` (no A2A).
- `docs/research/2026-06-30-kong-ai-gateway-fit.md` + `docs/research/2026-07-02-auto-model-routing-vs-clickup-kong.md` — prior Kong verdicts (superseded/operationalized here); `docs/research/2026-07-10-shared-brain-org-context.md` — MCP stays local, server zero-knowledge.
