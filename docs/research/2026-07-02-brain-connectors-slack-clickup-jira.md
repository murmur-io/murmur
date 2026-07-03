<!-- Generated 2026-07-02 via /research (murmur-researcher fan-out: class prior-art / per-connector feasibility / seam design). Consolidates the 2026-07-01 MCP-connectors brief. API shapes + pricing = point-in-time mid-2026. -->
# Research: Preparing the brain to connect to Slack / ClickUp / Jira for information search

## TL;DR / Verdict

**We are already "prepared" — the connector seam exists, ships, and is source-agnostic.** Adding Slack/ClickUp/Jira is a *clone of `connectors/web.rs`* (~8 files, effort **M**, low novelty), NOT a re-architecture. The shipped `Connector` trait (`egress_class()` + `async search(redacted_query) -> Vec<ConnectorHit>`), the `ConnectorRegistry` that redacts the outbound query + consent-gates fail-closed at the boundary, the per-connector enable+consent+Keychain-key pattern (`web_search_*`), the tool-wiring (`web_search` ToolSpec → `GatedToolExecutor` → cited hits), and the FE consent toggle are all proven by two live connectors (`web` = External, `calendar` = Local).

**Three decisions from the fan-out:**
1. **Pattern: live, user-token, read-first tools — NOT crawl-and-index.** Our recorded stance ("connectors = live tools the brain calls, only owned notes get vectors") is now the *documented industry norm* for user-level data — Microsoft splits connectors into "synced/indexed" (org-level) vs "federated/live" (user-level, MCP-based, read-only, don't index); the federated column is a near-exact spec of what we should build. Permission-awareness comes **free** for a single-user tool (your own token sees exactly what you can see — the thing ClickUp *acquired Qatalog for $25.4M* to fake for multiplayer).
2. **First connector: native REST, paste-a-token — Slack or Jira, NOT ClickUp.** ClickUp's public REST has **no full-text search** (structured filters only) → a native connector literally cannot search it; ClickUp needs the OAuth-2.1 MCP-client path (defer). Slack (`search.messages` + pasted `xoxp-` user token) and Jira Cloud (`/rest/api/3/search/jql` + email:API-token basic auth) are both single authenticated REST calls that map cleanly to `ConnectorHit`.
3. **The one real design gap = inbound-hit egress.** Outbound queries are already redacted by the framework. But Slack messages / Jira tickets pulled back and re-fed to a **cloud** reasoner egress to the cloud model — and today they ride only the **regex** firewall (emails/cards/phones), which does NOT scrub names, project codenames, or ticket prose (`NoopNameRedactor` is the default). Same posture web hits already have (not a *new* hole), but connector content is far likelier to be sensitive. **Must be made loud in consent copy; the DeBERTa NER seam exists for a later hardening.**

**Recommendation:** ship **Slack first** as a native REST connector (highest demand — "what did we decide in #channel?" — cleanest citations via permalinks), Jira second, ClickUp only via the deferred MCP-client path. Scope = **read/search only** (the user asked "aby szukać informacji"); write-out (create-issue via propose-accept) is a separate, higher-value/higher-risk track. **RAG bake-off stays gate #1** — connectors are the highest-value *additive* feature, they don't answer whether the core brain needs work.

## Co już mamy (z repo — cytuję symbol, pliki są duże)

- **Seam + framework** (`connectors/mod.rs`): `Connector { id, egress_class, async search }`; `EgressClass::{Local, External}`; `ConnectorHit { title, snippet, url, source_label }`; `ConnectorError::{NeedsConsent, Unconfigured, Failed}`. `ConnectorRegistry::search` **redaguje wychodzące query przez firewall ZANIM konektor je zobaczy** (test `registry_redacts_query_before_the_connector_sees_it`) i consent‑gate'uje na granicy (fail‑closed: niezconsentowany konektor jest NIEOBECNY w rejestrze → brain nie oferuje toola). Nagłówek pliku: *"Connectors are LIVE tools, NOT vectorized"* — nasza decyzja, w kodzie.
- **Szablon do sklonowania** (`connectors/web.rs`): `WebConnector::from_config_if_available` = fail‑closed gate (enabled && consented && klucz w Keychain, else `None`), sub‑seam `WebSearchProvider` (Brave, swappable), network‑free `parse_results` z testem `brave_parser_maps_json_to_hits`, głośny `source_label` ("web · Brave"). Używa in‑tree rustls `reqwest` → **zero nowych zależności**.
- **Dowód na structured‑snippet** (`connectors/calendar.rs`, `hit_for`): wielopolowy blok Meeting/When/Attendees/Agenda pakuje się w `ConnectorHit.snippet` bez zmiany schematu — dokładnie szablon dla ticketu Jira / wątku Slack.
- **Consent/klucz** (`settings/config.rs` + `commands.rs`): `web_search_enabled` + `web_search_consented` (**preserve‑only**, flipowany tylko przez `consent_to_web_search`; test `save_config_merge_never_clobbers_or_grants_web_search_consent`) + Keychain `web_search_api_key` via `set_web_search_api_key`/`has_web_search_key`, zarejestrowane w `lib.rs generate_handler!`.
- **Tool‑wiring** (`tools.rs`): `ToolCall::WebSearch` + `tool_specs()` entry + `execute_web_search` + `format_web_hits` + `GatedToolExecutor::specs()` filtr (wymaga `has_app`) + `run()` arm. Sync `execute_tool` **ODMAWIA** egress‑konektorom (`WebSearch => Err(InvalidArg)`, test `sync_execute_tool_refuses_websearch`) → read‑only MCP surface nigdy nie egresuje.
- **Inbound‑hit egress path** (`agent.rs run_agentic_loop` → `CloudReasoner::reason` → `make_provider` → `RedactingProvider::complete_with_meta`): re‑fed tool output redaguje BOTH system+user przez `redact_into` + wpis do egress‑ledgera — ale scrubbery to tylko `email_re`/`card_re`/`phone_re` (`redact.rs`); nazwy = `NoopNameRedactor` domyślnie.
- **NIE zwektoryzowane** (potwierdzone `grep`): konektory nie dotykają `vec_chunks`/`index_doc`/`upsert_embedding` — efemeryczne, re‑fetchowane per ask, nigdy w SQLite. Tylko owned notes/docs dostają wektory.
- **Jesteśmy MCP *serwerem*, nie klientem** (`mcp.rs` = ręczny JSON‑RPC over `tiny_http`); **brak kodu MCP‑klienta i crate'a** w drzewie. Bycie MCP‑klientem = net‑new architektura.

**Czego NIE mamy:** żadnego konektora Slack/Jira/ClickUp, żadnego OAuth flow. Slack jest stub'em w `voice_action.rs` ("not available yet").

## Findings (per kąt)

### Kąt 1 — Wzorzec klasy: live/federated dla user‑level, index dla owned; nasza pozycja = norma, nie kontra
- **Microsoft Copilot** rozdziela konektory na **synced/indexed** (org‑level, crawl do Graph indexu) vs **federated/live** (user‑level, **MCP‑based, read‑only, OAuth, bez indeksowania** — "ideal for live/dynamic/sensitive data that shouldn't be indexed"). Federated = niemal dokładny spec tego, co mamy budować. (high — learn.microsoft.com, 2026‑05‑14)
- **Glean** (czysty indexer) argumentuje przeciw pure‑live: federacja "tylko tak szybka jak najwolniejszy system", traci cross‑source ranking; ich eval: off‑the‑shelf MCP/federated **~30% więcej tokenów, do 2×** (83k vs 43k) bo każde źródło to osobny tool call + over‑fetch. (medium‑high, vendor‑self‑serving, ale mechanizm zgodny z first‑principles). **Implikacja: nie fan‑outuj do N konektorów per ask** — brain woła JEDEN właściwy tool do JEDNEGO pytania (nasza pętla już tak robi); trzymaj `max_steps` niski i budżet re‑fed wyników ciasny (już jest `RESULT_BUDGET`).
- **Perplexity** robi oba per‑konektor: GDrive indexed, Slack live/not‑stored (kontekst kasowany po query). Nasz split (wektory dla owned, live dla external) = podręcznikowy hybrid, **zwalidowany**.
- **Permission‑awareness za darmo dla single‑user:** Slack MCP — "AI only sees what you can see"; Atlassian — "the agent does not get to do more than the human whose OAuth token it's using"; ClickUp **kupił Qatalog za $25.4M** żeby to udawać dla multiplayer. Break case = tylko shared workspaces + cache'owanie wyników (którego NIE robimy). (high)
- **Skala dla naszego ICP:** solo dev/konsultant na macOS. Konektory które się liczą = narzędzia w których ta osoba żyje. ClickUp Brain MAX indeksuje GDrive/Figma/GitHub/SharePoint/Slack/Dropbox ale **NIE Linear/Jira/Notion/Asana** → nisza dev otwarta. NIE gonić: Notion (dubluje vault), CRM (zły ICP), org‑level indexed enterprise search (multiplayer, sprzeczne z local‑first).

### Kąt 2 — Feasibility per‑konektor (native REST vs MCP‑client)

| | **Slack** | **Jira (Cloud)** | **ClickUp** |
|---|---|---|---|
| **Search endpoint** | `search.messages` (legacy ale działa); nowe `assistant.search.context` (Feb 2026) = przyszłość | `POST /rest/api/3/search/jql` z JQL. **Stare `/rest/api/3/search` USUNIĘTE → 410** (~paź 2025) | **BRAK full‑text search w REST** — tylko structured filters. Search istnieje **tylko w MCP serwerze ClickUp** |
| **Maps to ConnectorHit?** | Tak, czysto (`text`→snippet, `permalink`→url, `channel.name`+`username`→label) | Tak (`key`+`summary`→title, browse URL, pole→snippet) | Częściowo przez REST (browse‑not‑search); MCP search mapuje dobrze |
| **Auth (najniższe tarcie)** | **Wklej `xoxp-` user token** (self‑install single‑workspace app → "Install to Workspace" → copy token; **brak callback servera**) | **Wklej email + API token** (Basic auth base64 `email:token`) | Personal `pk_…` (wklej) ale **nie umie search**; MCP wymaga OAuth 2.1+PKCE |
| **Scope** | `search:read` (user token, NIE bot token) | dziedziczy uprawnienia usera | pełny dostęp usera |
| **Rate limit** | Tier 2 ~20/min (OK dla on‑demand) | 429 + `Retry‑After` | 100/min free |
| **Official MCP?** | `mcp.slack.com`, **OAuth‑only, remote** | `mcp.atlassian.com/v1/mcp`, **OAuth 2.1 LUB API token** | `mcp.clickup.com/mcp`, **OAuth 2.1+PKCE only, beta** |
| **Native‑REST effort** | **S/M** | **M** | **niewykonalne przez REST** (L via MCP) |

- **Wszystkie oficjalne MCP serwery = remote‑hosted SaaS, dwa OAuth‑2.1‑only** (Slack, ClickUp). "Murmur jako MCP klient" ≠ mniej kodu tą samą prywatnością — to (a) taniec OAuth 2.1+PKCE z zarejestrowanym redirectem i (b) **query i wyniki usera transitują przez trzeci serwer** (nie tylko API źródła). Rust MCP klient (`rmcp`) istnieje ale to **nowy crate** (wymaga zgody) + OAuth. **Native REST wygrywa dla Slack/Jira; MCP‑client tylko dla ClickUp** (gdzie REST nie umie search).
- **Licensing:** brak blokera dla third‑party desktop app wołającej te API **tokenem usera** — to zamierzone użycie personal tokenów. Zero redystrybucji vendor code.

### Kąt 3 — Design dopasowania do seamu
- **Trait wystarcza as‑is** — NIE dodawać drugiej metody ani typowanych wyników. Jira issue → `title:"PROJ‑123 Fix login"`, `snippet:"Status: In Progress\nAssignee: Anna\n\n<excerpt>"`, `url:<browse>`, `source_label:"Jira"`. Calendar już dowodzi structured‑in‑snippet. Bounduj snippet (~1‑2 zdania + kluczowe pola) — pętla już truncuje re‑fed output.
- **Consent & klucze = per‑konektor keys** (mirror `web`, nie generyczny registry — preserve‑only consent to ręcznie audytowany inwariant): `slack_search_enabled`/`slack_search_consented` + Keychain `slack_token`; Jira/ClickUp też base‑url/email (non‑secret config, nie Keychain).
- **INBOUND‑HIT EGRESS = kluczowa decyzja prywatności:** wychodzące query redagowane za darmo (framework). Wyniki (Slack msg / Jira body) re‑fed do **cloud** reasonera egresują — dziś przez `RedactingProvider` (system+user oba redagowane), ale **tylko regex** (maile/karty/telefony), NIE nazwy/kryptonimy/prozę. **Identyczne z obecnym web/related_context** — nie nowa dziura — ale konektor‑content dużo częściej wrażliwy. **Wymóg: głośna consent copy ("treść Slack/Jira którą pobierasz jest wysyłana do Twojego modelu chmurowego, PII‑scrubbed") + egress‑ledger (już strzela).** Silniejszy scrubbing = osobny projekt (seam DeBERTa NER `ner_deberta.rs` istnieje).
- **Full wiring checklist (~8 plików, mirror web_search):** (1) `connectors/jira.rs` + `from_config_if_available` + `parse_results`; (2) `mod.rs` `pub mod` + linia w `ConnectorRegistry::build`; (3) `tools.rs` — ToolCall variant + execute_tool refusal arm + tool_specs + dispatcher + format_ + specs filtr + run arm; (4) `config.rs` — enabled/consented + base_url/email + K‑consts + load/save + grant fn; (5) `commands.rs` — consent_to_ + set_token/has_key; (6) `lib.rs` — 3 rejestracje; (7) FE — ipc.service + models DTO + settings toggle/key/consent; (8) testy — parser, fail‑closed registry, sync refusal, consent‑no‑clobber, format‑is‑loud.
- **Live‑tool, not vectorized — potwierdzone, zero tension:** konektory nic nie piszą do DB, omijają cały seal/lock model (external data nigdy at‑rest w Murmur). **Lock:** ask mieszający external + owned nadal idzie przez `GatedToolExecutor` którego vault‑toole visibility‑gate'ują per‑call → locked meeting nie wycieknie tą samą odpowiedzią; external leg nie dodaje bypassu.

## Fit z ograniczeniami Murmur
- **Local‑first:** konektor opt‑in, fail‑closed, absent‑until‑consented → default install egresuje zero. ✅ (z głośnym caveatem inbound‑redakcji)
- **Redaction firewall:** query redagowane przez framework; inbound przez `RedactingProvider` na drodze do cloud reasonera — **ten sam seam, bez bypassu**. ✅ Strain: regex nie scrubuje nazw/kryptonimów — głośno w consent copy; NER = osobny future.
- **SQLite‑canonical:** konektory nic nie piszą; efemeryczny cited kontekst. ✅
- **macOS / no‑new‑deps:** czysty in‑tree `reqwest`, zero nowych crate'ów (native REST). MCP‑client path wymaga `rmcp` → zgoda usera. ✅
- **CI honesty:** w pełni unit‑testowalne offline (fake provider + fixture parser jak brave). Live token round‑trip = ręczny smoke ("needs a real token"), nie "needs a real Mac" — lżejszy bar. Write‑carve‑out (jeśli kiedyś) = wymagany `lock-security-reviewer`.

## Opcje i tradeoffy
- **A — Slack first, native REST, read‑only (S/M, low risk).** Wklej `xoxp-` token; `search.messages` → `ConnectorHit` z permalinkami. Największy demand ("co ustaliliśmy w #eng?"), najczystsze cytaty. Dług: `search.messages` legacy (przyszła migracja do Real‑time Search API jeśli Slack usunie).
- **B — Jira second, native REST (M, low‑med risk).** `/search/jql` + email:token basic auth; obsłuż `nextPageToken` + nie kopiuj usuniętego legacy endpointa. Dla Atlassian‑shop userów równorzędny pierwszy wybór.
- **C — ClickUp via MCP‑client (L, med‑high risk).** Jedyna droga (REST nie umie search): `rmcp` (nowy crate) + OAuth 2.1+PKCE + remote SaaS w pętli + beta serwer z churning toolami. Odłóż do realnego demandu; wtedy daje też generyczny MCP‑konektor shape.
- **D — write‑out (create Jira/Linear issue via propose‑accept) — osobny track.** User pytał o *szukanie*; write to wyższa wartość ale lethal‑trifecta (broad token + untrusted inbound + egress) → read‑only first, human‑in‑loop na write, wymagany lock‑security. Poza scope tego researchu.
- **Odrzucone:** crawl‑and‑index‑external (łamie local‑first, rozwiązuje multiplayer problem którego nie mamy); generyczny connector‑registry config przy N=3 (przedwczesna abstrakcja — wróć przy N≥4).

## Rekomendacja i pierwszy krok
**Odpowiedź na "chyba że już jest": seam JEST i jest source‑agnostic — architektonicznie jesteśmy gotowi; brakuje samych implementacji, a każda to klon `web.rs`.** Zbuduj **Slack jako pierwszy native REST konektor** (read‑only), Jira drugi, ClickUp tylko przez odłożony MCP‑client.

Najmniejszy weryfikowalny slice (mirror `brave_parser_maps_json_to_hits`):
1. **(bez sieci)** `SlackSearchProvider::parse_results(fixture_json) -> Vec<ConnectorHit>` + unit test mapowania (title/permalink/channel‑label). RED‑before‑GREEN, zero credentiali — to cały techniczny risk.
2. Real client za `from_config_if_available` (enable+consent+token) + `execute_slack_search` dispatcher + ToolSpec + config/consent/Keychain + `lib.rs` regs + FE toggle.
3. **Przed merge:** głośny inbound‑egress caveat w consent copy + potwierdź że egress‑ledger strzela na connector‑hit cloud call.

De‑risking spike: parser‑test + jeden ręczny `curl` z realnym `xoxp-` tokenem żeby zamrozić JSON shape + potwierdzić że fail‑closed gate trzyma gdy niezconsentowany.

**Sekwencja:** konektory to wartość *additive* — RAG bake‑off zostaje bramką #1 (czy sam rdzeń brainu jest OK); konektory to najwyższa‑wartość additive feature PO zmierzeniu brainu.

## Otwarte pytania / czego nie udało się zweryfikować
- Czy regex‑only redakcja jest *wystarczająca* dla enterprise Slack/Jira content — mechanizm działa (zweryfikowane), ale czy scrub tylko maili/kart/telefonów (nie nazw/kryptonimów) pasuje do threat‑modelu usera = decyzja produktowa. DeBERTa NER seam istnieje na później.
- Dokładny JSON envelope `search.messages` (zagnieżdżenie `messages.matches`, `channel.name` vs `.id`) — lista pól z docs, nie z live body; parser‑test rozstrzyga przed shipem.
- Czy Slack *usunie* `search.messages` (legacy, "don't use", ale działa) — timeline nieznany; jeśli usunie, A migruje do `assistant.search.context` (świat bot‑action‑token). Point‑in‑time 2026‑07.
- Jira Cloud dokładne rate‑limity — Atlassian nie publikuje (429 + Retry‑After).
- ClickUp MCP tool stability — public beta, surface churnował 6→49 przez 2026.
- `rmcp` API churn + reqwest 0.12↔0.13 pin (go/no‑go generycznego MCP klienta) — odłożone do Phase‑2 build‑proof spike'u (patrz `2026-07-01-mcp-connectors-slack-jira-linear.md`).
- Prompt‑injection resistance, live OAuth round‑trips, realne Slack/Jira calls — NIE headless‑provable; realny Mac + realne workspace'y + red‑team harness (recorded). Honesty bar.

## Sources
**Zewnętrzne (point‑in‑time mid‑2026):** learn.microsoft.com/microsoft-365/copilot/connectors/overview · glean.com/blog/federated-indexed-enterprise-ai · glean.com/blog/cowork-mcp-eval · perplexity.ai/enterprise/app-connectors · businesswire.com (ClickUp↔Qatalog $25.4M) · docs.slack.dev/reference/methods/search.messages · docs.slack.dev/ai/slack-mcp-server · docs.slack.dev/apis/web-api/real-time-search-api · developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issue-search · developer.atlassian.com/cloud/jira/platform/basic-auth-for-rest-apis · github.com/atlassian/atlassian-mcp-server · developer.clickup.com/reference/getfilteredteamtasks · developer.clickup.com/docs/connect-an-ai-assistant-to-clickups-mcp-server · github.com/modelcontextprotocol/rust-sdk (rmcp) · simonwillison.net/2025/Jun/16/the-lethal-trifecta
**Wewnętrzne:** `connectors/mod.rs` (trait/registry/redakcja‑na‑granicy) · `connectors/web.rs` (`from_config_if_available` + `WebSearchProvider` + `brave_parser_maps_json_to_hits` — szablon) · `connectors/calendar.rs` (`hit_for` — structured‑in‑snippet) · `tools.rs` (`execute_web_search`/`GatedToolExecutor`/sync‑refusal) · `agent.rs` (`run_agentic_loop` — inbound‑egress path) · `reason.rs` (`CloudReasoner`→`RedactingProvider`) · `summarize/redact.rs` (`complete_with_meta` + regex scrubbers, names no‑op) · `settings/config.rs` (`web_search_*` wzorzec) · `commands.rs` (`consent_to_web_search`/`set_web_search_api_key`) · `secrets/keychain.rs` (`WEB_SEARCH_KEY_ACCOUNT`) · `mcp.rs` (jesteśmy serwerem, brak klienta) · `docs/research/2026-07-01-mcp-connectors-slack-jira-linear.md` + `docs/research/2026-07-02-clickup-brain-gap-analysis.md`
