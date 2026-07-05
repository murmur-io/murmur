<!-- Generated 2026-07-05 via /research (murmur-researcher fan-out: RAG-vs-live decision / competitor index-vs-live architectures / verification-as-a-profile). Builds on the 2026-07-01 + 2026-07-02 connector-architecture briefs (which cover the "how to wire it" question). Vendor facts/pricing/APIs = point-in-time mid-2026. -->
# Research: Connectors (Slack / Jira / ClickUp / Linear) — should the brain draw from them LIVE, or store their data as a local RAG?

## TL;DR / Verdict

**Keep connectors LIVE, per-source, as gated on-demand tools — do NOT stand up a persistent local RAG over Slack/Jira/ClickUp/Linear.** All three research angles converge on the same answer, and it *confirms* our recorded decision (`connectors/mod.rs:9` — "Connectors are LIVE tools, NOT vectorized"). The one-line reason: **Murmur is single-user and local-first, which collapses the entire cost that makes indexing worthwhile for everyone else to zero.** The user's own OAuth token to Slack/Jira/Linear *is* the complete ACL — a live query returns exactly what they may see, always current, with nothing to mirror, invalidate, seal, or leak.

Three findings, one per angle:

1. **The architecture is a solved, industry-canonical split.** Microsoft states it cleanest: **synced/indexed = org-level, admin-crawled, ACL-mirrored** vs **federated/live (MCP) = user-level, user's-own-token, read-only, no indexing, for "live/dynamic/sensitive data."** Perplexity ships exactly our split (Drive *indexed*, Slack *live/deleted-post-query*); ClickUp *acquired* Qatalog ($25.4M) specifically for a **no-index, permission-aware** engine. Everyone who indexes must build + continuously re-sync a mirrored ACL (Glean webhooks + re-crawl; Onyx 30-min Celery permission-sync, gated behind Enterprise). Everyone who goes live gets permissions + freshness free from the source token. Our "owned notes get vectors / connectors stay live" is the *documented norm*, not an idiosyncrasy.

2. **The "verify" use-case is the single strongest pro-LIVE argument.** The user named two modes — *"o coś pytam"* (ask) and *"coś weryfikuję"* (verify). Staleness degrades ask *gracefully* (slightly old context) but **inverts verify** (a ticket rescheduled yesterday, checked against a week-old index, returns a confident "✓ confirmed" — a false trust signal the user acts on). A wrong "confirmed" is worse than no answer. Verification is precisely the case the RAG-staleness literature calls most dangerous, because the output *is* the trust judgment.

3. **We already have the machinery for both modes — and for verification done *safely*.** "Ask" = the agentic loop (`agent.rs::run_agentic_loop`). "Verify" = the deterministic-judge pattern already shipped in `facts.rs::reconcile_facts` + `brain_reactions.rs` ("the LLM NEVER judges … the VERDICT is deterministic") + the non-destructive `> unverified` markers of `grounding.rs::annotate_unverified`. Pointing that verification engine's *source of truth* at a live connector (instead of a past meeting) is a smaller, safer build than a RAG index — and it neutralizes prompt-injection (the untrusted ticket text is only *extracted from*, never *judged by*).

**Recommendation:** ship all four connectors LIVE (each a clone of `connectors/web.rs`, effort S–M/source) with a **RAM-only, request-scoped hit cache** to tame latency/rate-limits, and **user-pin-to-note** as the *only* path connector data ever enters the vector store (at which point it becomes owned content and rides the existing sealed/gated doc-ingest pipeline). Revisit a *scoped* local index **only for ClickUp**, **only if** its weaker search API measurably underperforms in practice. First slice = **Jira** (strongest native search + mutable fields that fit the verify/reconcile model best).

## Co już mamy (z repo — cytuję symbol; pliki są duże)

- **Connector seam — LIVE, shipped, source-agnostic** (`connectors/mod.rs`): `trait Connector { id, egress_class, async search(redacted_query) -> Vec<ConnectorHit> }`; `ConnectorRegistry::search` redacts the outbound query through the FULL firewall (regex + on-device NER names) *before* the connector sees it, records one content-free egress-ledger row per attempt, fails closed when unconsented. `EgressClass::External` connectors exposed only when `enabled && consented && keyed`. Live today: `web` (Brave), `calendar` (Local). Slack/Jira/ClickUp/Linear **not implemented** — each is a `web.rs` clone (per the 2026-07-01/-07-02 architecture briefs).
- **Vector/RAG stack — real, scoped to OWNED content only:** two vec0 KNN stores, both 384-dim (`embed.rs` `EMBED_DIM`): `vec_chunks` (note+transcript, `db.rs`) + `doc_vec_chunks` (uploaded docs/typed notes). Embedder = **multilingual-e5-small** (candle BERT / Metal, downloaded-not-bundled, model-presence gated → `StubEmbedder` fallback); `mmlw-retrieval-e5-small` (Polish) selectable, same dim. `semantic_search_enabled` defaults **ON**.
- **The lock invariant any indexed connector data would collide with:** chunks/vectors are *"invertible PII derived from plaintext, so they exist ONLY for VISIBLE documents — PURGED in the same transaction that seals a folder, re-embeddable on unlock"* (`db.rs`). The `documents` table is anchored to a `folders` row (`folder_id NOT NULL` = the lock gate). Connector rows have **no folder to anchor to** → indexing them forces either a *new* lock domain or an *ungated* leak (fails `lock-security-reviewer`).
- **Three deterministic-verdict verification engines already shipped** (all read *internal* sources): `grounding.rs::annotate_unverified` (note-vs-transcript, zero-egress, `> unverified` markers); `facts.rs::reconcile_facts`+`extract_fact_candidates` (bitemporal, pure-function verdict, LLM extract-only); `brain_reactions.rs::detect_reactions` (far-side utterance → triple → `reconcile` → Contradiction card citing the OLD fact — "the whisper").
- **Zero-egress proactive path** (`proactive.rs`, contract D1: no LLM/provider/consent/egress) — verification against a connector **egresses**, so it must stay a separate, consented, on-demand surface, NOT bolted into proactive/reactions.
- **RAG bake-off** (`docs/research/2026-07-04-rag-bakeoff-results.md`): semantic recall@5 **1.00 vs FTS 0.42** on 12 Polish paraphrase queries over a 43-meeting corpus — but the doc flags it was a *test graveyard* with a query mix *designed* to expose the gap; vectors earn their keep **for owned, stable notes on paraphrase** — a finding that does **not** transfer to volatile external data.

## Findings

### Angle 1 — RAG vs live, the trade-off matrix (each concrete for Murmur)

| Trade-off | LIVE | LOCAL INDEX (RAG) |
|---|---|---|
| **Freshness (decisive)** | Always current. Correct for *verify*. | Stale between syncs → **inverts verify** ("confirmed" against an obsolete value). |
| **ACL / permissions** | Source enforces via the user's token — free, always-correct, instantly-revocable. | Must reconstruct + continuously re-sync ACLs; permission change = delete-243 invalidation; a leak surface. |
| **Lock model** | Nothing at rest → nothing to seal/gate. | No folder anchor → invent a new lock domain or leak. New at-rest SQLCipher copy of company data. |
| **Sync/invalidation** | Zero — no scheduler. | Webhooks/polling + delta sync + deleted-ticket/edited-msg/moved-task handling = permanent debt. |
| **Privacy/egress** | Redacted query egresses on demand + ledgered; nothing stored. | Embedder is on-device (no embed egress) — but a fresh at-rest copy of Slack/Jira lands locally. |
| **Retrieval quality** | Bounded by the source's search API (weak for ClickUp). | Cross-source semantic ranking + rescues weak APIs — but under-indexes at solo-consultant scale. |
| **Cost/complexity** | Clone `web.rs`, effort **S–M/source**, reuses redaction/consent/ledger free. | New tables + embed pipeline + sync scheduler + ACL mirror + lock integration, effort **L + ongoing**. |

- **Per-source search-API strength changes the calculus source-by-source** (this is the one nuance that isn't uniform):
  - **Jira** — JQL `text ~ "..."` = Lucene full-text over summary/description/comments; fuzzy/wildcard/proximity. **Strong** (high conf).
  - **Slack** — `search.messages` full-text, user-token only (not bot), respects channel membership. **Good** (med).
  - **Linear** — GraphQL content search across title/description/comments + a built-in similarity/vector search. **Decent** (med).
  - **ClickUp** — `Get Filtered Team Tasks` = structured filters + text match on **title/description only**, no relevance ranking, no comments/docs full-text. **Weakest** — functional but the *only* plausible local-index candidate (med). (The old "ClickUp has NO full-text search" line is directionally right but not literal.)

### Angle 2 — What competitors actually do (index vs live)

| Player | Model | ACL mechanism | Level |
|---|---|---|---|
| **Glean** | Index | ACL **mirrored** via webhook + re-crawl | Org |
| **Onyx (Danswer)** | Index (Postgres/Vespa→OpenSearch) | ACL **mirrored** via 30-min Celery permission-sync (Enterprise-only) | Org |
| **Dust** | Index | mirrored, Temporal event-sync | Org |
| **MS Copilot — synced** | Index | mirrored in Graph | Org (admin creds) |
| **MS Copilot — federated** | **Live/MCP** | **source enforces, user token** | **User** |
| **Perplexity — Drive** | Index (S3 + Vespa) | mirrored, dynamic revoke | Org/User |
| **Perplexity — Slack** | **Live** | **source enforces, context deleted post-query** | **User** |
| **Qatalog / ClickUp Brain²** | **Live / no-index** | **source enforces at query (ActionQuery)** | Org (permission-aware) |

- **The framework is unambiguous:** everyone who indexes must build + continuously refresh a **mirrored ACL** — Onyx gates it behind the *Enterprise* tier and its own tracker shows the cost (issue #9664: Slack permission-sync "iterates entire channel history"). Everyone who goes live gets permissions + freshness free from the source token, and live is chosen *precisely for* dynamic/sensitive/user-scoped data.
- **Single-user is the decisive variable.** The index tax = freshness-invalidation + **ACL-mirror-sync** + storing others' data. In multiplayer the ACL mirror is unavoidable and expensive; **for one user, the user's own token IS the complete ACL** (Perplexity-Slack's "immediate revocation, context deleted" property, by construction).
- **Glean's pro-index case doesn't apply at our scale:** cross-user popularity signals, unified cross-source ranking over millions of docs — all *multiplayer* advantages. None applies to a single-user vault. Their token-tax numbers (43k vs 83k, ~2.5× preferred) are competitor-marketing — directionally plausible, not independently verified.
- **Live-first works at scale:** three independent players ship it successfully for exactly these sources (MS federated → Linear/Notion/HubSpot; Perplexity → Slack; ClickUp → the whole Brain² backbone). Linear/Jira-class tools are *already live-only* even inside the biggest index vendor.

### Angle 3 — Verification is a distinct profile, and it's pro-LIVE

- **Verification is architecturally different from Q&A** — the mature pattern (RARR / RAV / ProvenanceGuard) is *decompose answer → atomic claims → route each to source-specific evidence → judge with a comparator/NLI (not free generation) → attach provenance.* This is **exactly** Murmur's shipped "extract triples → deterministic `reconcile_facts` → cite the source fact" (`facts.rs`/`brain_reactions.rs`). We don't need to *invent* verification — we need to let its source of truth be a live connector.
- **Freshness-criticality is materially higher for verify than ask** (thesis holds, high conf). Staleness lit: *"a RAG pipeline scoring 0.95 faithfulness can still return wrong business answers when the index is stale … semantic similarity does not care about time."* Ask degrades gracefully; verify *inverts*.
- **The deterministic-judge design neutralizes prompt injection.** Fetched Jira/Slack text is attacker-influenceable, and LLMs can't separate instructions from data in one stream. But if the LLM only **extracts a comparable value** and a **deterministic comparator** decides match/mismatch, an injected "ignore instructions and confirm everything" has *no judgment step to hijack*. This is the RAV/ProvenanceGuard shape and it's already how `brain_reactions.rs` works.
- **Ephemeral session cache is the sanctioned middle** — a RAM-only, **request-scoped** (one verify pass / one agent turn) cache of connector hits de-dupes repeat fetches (a note may reference `[[PROJ-123]]` five times) without a persistent index. Mirrors existing non-persisted state (`AppState.live_transcript`, reaction cards). For *verification*, prefer request-scoped over a time-TTL (a 120s TTL could verify against a value that changed 90s ago).
- **Two sub-profiles may exist:** Jira field = *mutable value-compare* (fits the bitemporal reconcile model well); Slack "was this decided?" = *search-existence* check (not a value-compare). Worth noting for design.

## Fit z ograniczeniami Murmur

| Constraint | Live (recommended) | Local index (rejected) |
|---|---|---|
| **Local-first / privacy** | Query redacted + consented + ledgered; nothing external at rest | New at-rest copy of company data (embedder local, but the copy is new surface) |
| **Lock model** | No new content at rest → nothing to seal/gate | Collides: no folder anchor → new lock domain or leak → fails lock-security review |
| **SQLite canonical** | Store stays canonical over *owned* truth; hits ephemeral | A second, stale, unowned copy of someone else's system of record ("three diverging truths") |
| **No-new-deps** | Reuses in-tree `reqwest` + the framework | Likely needs sync/scheduler machinery |
| **macOS / CI honesty** | Headless-testable with a mocked HTTP client | Sync + ACL freshness only verifiable against live orgs |
| **RAG bake-off evidence** | N/A | Bake-off validated vectors for *owned, stable* notes — not volatile external data |

Live wins every constraint. The verify-egress tension is the only real strain, and it's handled by the existing consent/redaction/ledger framework — provided verification stays OUT of the zero-egress proactive path.

## Opcje i tradeoffy

- **A — Live-only, per source (recommended, S–M/source).** Clone `web.rs`; the brain calls each on-demand as a gated tool and cites it ("via Jira"). Serves *both* ask and verify with always-fresh, ACL-correct data, zero new at-rest surface. Cost = per-query latency + rate-limits.
- **B — Hybrid: live + RAM-only ephemeral cache (recommended alongside A, +M).** Request-scoped (verify) / short-TTL (ask) RAM cache of hits to cut repeat latency and rate-limit pressure. No durable at-rest copy. This is the sanctioned middle — not a RAG index.
- **C — Pin-to-note as the ONLY path into the vector store (recommended alongside A, +M).** When the user *explicitly pins* a connector hit into a note, it becomes **owned** content and rides the existing `documents`/`import_text` sealed-gated-embeddable pipeline. No new lock domain; the user's deliberate action is the ACL decision.
- **D — On-demand connector-verify pass (recommended follow-on, M).** Reuse `reconcile_facts` + `annotate_unverified` markers: per note claim tied to a `[[wikilinked]]` entity/ticket → fetch live → LLM *extracts* the comparable field → **deterministic** compare → append non-destructive `> ✓ confirmed in PROJ-123 (In Progress)` / `> ⚠ not found` / `> ⧗ conflict: note says Fri, PROJ-123 due Wed`. On-demand + consent-gated, never proactive.
- **E — Full local RAG index of connectors (rejected, L + ongoing).** Walks straight into staleness-inverts-verification, an at-rest plaintext shadow of external data (lock strain), a mirrored-ACL subsystem, and a second source of truth — the Onyx/Glean cost with none of the multiplayer benefit.
- **Rejected also:** generic connector-registry config abstraction at N=4 is fine to defer until a 5th source; crawl-and-index-external breaks local-first and solves a multiplayer problem we don't have.

## Rekomendacja i pierwszy krok

**Ship connectors LIVE (A) + RAM cache (B) + pin-to-note (C); add the deterministic connector-verify pass (D) as the differentiated follow-on; never build the RAG index (E).** This preserves local-first, doesn't fork the lock model, keeps SQLite canonical over *owned* truth, respects no-new-deps, matches the bake-off evidence, and correctly serves the "verify" use-case a stale index would silently break — while being the industry-canonical quadrant for single-user, freshness-critical, privacy-first, user-permissioned data.

**Per-source priority** (search-API strength × verify-fit × demand):
1. **Jira** — strongest native search (JQL Lucene) + mutable fields fit the reconcile/verify model best. Cleanest *end-to-end* first slice for both ask and verify.
2. **Slack** — high "what did we decide in #eng?" demand; good `search.messages` (user token). Search-existence sub-profile.
3. **Linear** — clean GraphQL + built-in similarity search; a proven MS-federated exemplar (confirms live suffices). Smaller install base.
4. **ClickUp** — live via `Get Filtered Team Tasks` (title/desc); **the one source to revisit for a *scoped* local index** *iff* live retrieval measurably underperforms. Don't build the index speculatively.

**Smallest verifiable first slice / de-risking spike:**
1. **(no network)** `JiraConnector::parse_results(fixture_json) -> Vec<ConnectorHit>` (JQL `text ~ query` result → hits) + unit test mapping (title = `PROJ-123 …`, browse URL, status/assignee in snippet), mirroring `brave_parser_maps_json_to_hits`. RED-before-GREEN, zero credentials — this is the whole technical risk of the connector.
2. Real client behind `from_config_if_available` (enable + consent + Keychain token) + `execute_jira_search` dispatcher + ToolSpec + config/consent/Keychain + `lib.rs` regs + FE toggle. Headless-verify: query redaction + consent-fail-closed + one egress-ledger row (mirror the `mod.rs` tests).
3. **Verify-profile spike (proves D + why freshness is load-bearing):** one headless test where a `reconcile`-style connector-verifier flips **confirmed ↔ conflict** when a *mocked* Jira field value changes for the same note claim. Ship it against ONE field (deadline or status) first.
4. Then dogfood ask + verify against a real Jira on a signed build. If live latency/recall is good (expected — JQL is strong), the decision generalizes to Slack/Linear; only ClickUp's result quality is worth re-measuring before ruling the index in/out for that one source.

**Sequencing honesty:** connectors are *additive* value. The RAG bake-off (does the *core* owned-note brain earn its keep on a real vault?) stays gate #1 — connectors don't answer whether the core needs work. After that, this is the highest-value additive feature.

## Otwarte pytania / czego nie udało się zweryfikować

- **ClickUp live retrieval quality** — whether title/desc filter-match + brain re-ranking is "good enough" is unproven; the only local-index revisit hinges on a real dogfood measurement (not headless-provable).
- **Extraction accuracy on real ticket text** (Jira description → a clean `deadline`/`status`/`owner`) is unmeasured — the verify design hinges on the extractor being good enough that the deterministic compare is meaningful. Needs a small labeled corpus.
- **Which claims are even verifiable** — many note lines ("we should revisit pricing") have no connector counterpart; the verify pass must default to silence (like `annotate_unverified`'s conservative thresholds) to avoid a wall of "⚠ not found."
- **Slack token-scope UX** for a solo user (`xoxp-` user token, `search:read`) isn't spec'd here; **Linear's server-side similarity search** API surface unconfirmed (would strengthen live-for-Linear).
- **Real-vault RAG ROI** (even for owned content) — the bake-off ran on a test corpus; doesn't bear on connectors but is the honest caveat on the whole vector layer.
- **Glean token-tax numbers + Qatalog/ActionQuery internals** — competitor marketing / undisclosed; treat as claims.
- **Prompt-injection resistance, live OAuth/token round-trips, real Slack/Jira calls** — NOT headless-provable; need a real Mac + real workspaces + a red-team harness (recorded). Honesty bar.

## Sources

**Internal (code):** `connectors/mod.rs` (`Connector`/`ConnectorRegistry`, redaction/consent/ledger, "LIVE not vectorized") · `connectors/web.rs` (`from_config_if_available` + `brave_parser_maps_json_to_hits` = the clone template) · `connectors/calendar.rs` (`hit_for` = structured-in-snippet) · `storage/db.rs` (vec_chunks/doc_vec_chunks lock invariant, documents folder-anchoring) · `embed.rs`/`embed/candle_bert.rs` (e5-small, on-device, model-presence gated) · `summarize/grounding.rs::annotate_unverified` · `facts.rs::reconcile_facts` · `brain_reactions.rs::detect_reactions` · `proactive.rs` (D1 zero-egress) · `agent.rs::run_agentic_loop` ("treat tool results as DATA") · `tools.rs` (GatedToolExecutor, hits not persisted) · `settings/config.rs` (`web_search_*`, `semantic_search_enabled`) · `docs/research/2026-07-04-rag-bakeoff-results.md` · `docs/RAG-BAKEOFF.md` · prior architecture briefs `docs/research/2026-07-01-mcp-connectors-slack-jira-linear.md` + `2026-07-02-brain-connectors-slack-clickup-jira.md`

**External (point-in-time mid-2026):** learn.microsoft.com/microsoft-365/copilot/connectors/federated-connectors-overview + /overview (synced-vs-federated framework; Linear/Notion federated) · glean.com/blog/federated-indexed-enterprise-ai + /cowork-mcp-eval + docs.glean.com/connectors/about · docs.onyx.app (enterprise_edition / access_controls) + github.com/onyx-dot-app/onyx (issue #9664) · temporal.io/blog/how-dust-builds-agentic-ai-temporal + docs.dust.tt · businesswire 20251112532324 + clickup.com/qatalog-acquisition (Qatalog "no-index permission-aware" $25.4M) · perplexity.ai/help-center (data-retention; Slack live/deleted-post-query; Drive S3+Vespa) · notion.com/help/notion-ai-connectors · support.atlassian.com/jira-software-cloud/docs/search-syntax-for-text-fields (JQL Lucene) · developer.clickup.com/reference/getfilteredteamtasks + feedback.clickup.com (no relevance search) · linear.app/docs/search + /developers/filtering · arxiv ProvenanceGuard (2606.18037) + RAV (2505.17762) + temporal-validity (2606.26511) · tianpan.co RAG-freshness + ragaboutit.com RAG-freshness-paradox · unit42.paloaltonetworks.com/ai-agent-prompt-injection + databricks.com/blog mitigating-prompt-injection · simonwillison.net/2025/Jun/16/the-lethal-trifecta
