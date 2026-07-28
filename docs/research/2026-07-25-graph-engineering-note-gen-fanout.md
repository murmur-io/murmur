<!-- Generated 2026-07-25 via /research (murmur-researcher fan-out, 4 angles). Pricing/rate-limits/competitor state = point-in-time. -->
# Research: "Graph engineering" u nas — czy warto zrobić cloud-path fan-out generacji notatki?

**Trigger:** public post about graph-style agent orchestration (linear workflow → graf: fan-out/parallel → barrier → fan-in, dynamic routing; "9/10 kroków nie musiało czekać"). Pytanie: czy i jak wprowadzić równoległą (fan-out) generację **sekcji** notatki tylko na ścieżce chmurowej, zachowując serializację on-device.

> **Aktualizacja 2026-07-28.** Ręczny `generate_digest` nie obcina już notatki
> w połowie ani nie pomija spotkań bez śladu: naprawił to zmergowany PR #451.
> Historyczny finding poniżej pozostaje użyteczny jako uzasadnienie decyzji,
> ale nie opisuje już tej ścieżki na trunku. Osobny scheduler
> `brief_runner::build_brief_corpus` nadal ma własny budżet i wymaga osobnej
> oceny; nie należy przenosić dowodu z PR #451 automatycznie na scheduler.

## TL;DR / Verdict

**Sekcyjny fan-out generacji notatki, jako oryginalnie zaproponowany, NIE jest wart budowy.** Cztery niezależne kąty zbiegły się:

- **Feasibility (A):** wykonalne warunkowo (seam czysty), ale tylko na `anthropic`; na domyślnym `claude_code` fan-out = N procesów Node = ryzyko OOM. Plus **landmina prywatności**: trzeba zsumować `CallMeta.redactions`, inaczej Privacy Receipt zaniża PII.
- **Jakość (B):** dla typowego spotkania Murmur (30–90 min, mieści się w kontekście) to **latency-win, nie quality-win**. Dowody na przewagę dekompozycji dotyczą tylko (a) DŁUGICH transkryptów (lost-in-the-middle) i (b) metod z retrievalem per-aspekt (AMTSum), a **nie** ślepych N-promptów na tym samym pełnym transkrypcie.
- **Koszt (C):** naiwny równoległy fan-out na `anthropic` = **~3,7× $**; **prompt caching NIE ratuje** prawdziwie równoległego fan-outu (cache dostępny dopiero po starcie pierwszej odpowiedzi). Note-gen to zadanie **w tle** — user na nie nie czeka, więc latency-win jest mało warty. Na `claude_code` (subskrypcja) = N× szybsze wypalanie rate-limitu + RAM.
- **Multi-meeting (D):** synteza wielu spotkań **już shippuje** jako
  single-pass. Historyczny silent-drop w ręcznym `generate_digest` został
  naprawiony w #451 (whole-note-or-skip + jawny licznik pominiętych spotkań).
  Nadal jest to właściwy reżim do oceny map-reduce, ale decyzja wymaga pomiaru
  obecnego zachowania obu ścieżek, w tym schedulera.

**Co jest warte roboty (w kolejności):** (1) **lepszy pojedynczy prompt** (per-section directives + chain-of-density na kompletność action-itemów) — tani quality-win teraz; (2) **eval-delta harness** żeby decydować danymi, nie wiarą; (3) jedyny fan-out wart eksploracji to **map-reduce digestu wielu spotkań**, zaczynając od pomiaru realnego drop-rate. Sekcyjny fan-out pojedynczej notatki — odłożyć/pominąć.

## Co już mamy (z repo, z file:line)

- **Note-gen ciała notatki = JEDNO wywołanie**, background job (flip do `Summarized`, nie foreground spinner): `pipeline.rs:2214-2279` (`provider.summarize_with_meta(&request)`), prompt wielosekcyjny w `summarize/template.rs:10` (`default_template`/`build_template`).
- **Action items nie są generowane osobno — są PARSOWANE** z markdownu notatki: `summarize/action_items.rs:14`. Recall action-itemów zależy dziś w 100% od jednego prompta ciała.
- **Seam fan-outu już istnieje i działa gdzie indziej:** timeline to osobne `complete_json` na żądanie (`commands/mod.rs:5422` → `summarize/timeline.rs:193`).
- **Multi-meeting synthesis JUŻ shippuje jako single-pass fan-in:** ręczny
  `commands/brief.rs::generate_digest` (Weekly Vault Digest) oraz scheduler w
  `brief_runner.rs`. PR #451 wydzielił dla ręcznej ścieżki
  `assemble_digest_corpus`: notatka wchodzi w całości albo jest pomijana, a
  corpus dostaje jawny licznik pominięć. Scheduler ma oddzielny assembler i
  nadal używa `take(remaining)`; jego zachowanie nie zostało naprawione ani
  empirycznie ocenione przez #451.
- **Twardy constraint:** `AppState::heavy_inference = Semaphore(1)` (`state.rs:374-385`) trzyma **tylko** krok NER-redakcji (`redact.rs:658`), NIE dyspozycję chmurową → fan-out chmurowy nie łamie ograniczenia, ale on-device i tak serializuje się (dowód: `local.rs:234`), więc fan-out on-device jest bezcelowy.
- **Firewall per-call:** każde wywołanie chmurowe jedzie przez `RedactingProvider::complete_with_meta` (`redact.rs:872`): redakcja + `acquire_external_egress_lease` (`:916`) + jeden `EgressEntry` (`:922`). Consent gated **przy konstrukcji** providera (`mod.rs:193`), fail-closed.
- **Eval infra gotowe do tej decyzji:** `eval/notes_bakeoff.rs:40,101` (`NoteMetrics` + `compare` A/B — komentarz `:14-18` szczerze mówi: proxy strukturalne, realna bramka = ślepy odczyt człowieka) + `eval/calibration.rs::coverage_fraction` (faithfulness: ułamek claim-linii z receiptem via `grounding::align_claims_to_segments`) + `summarize/action_items.rs` (ekstrakcja do liczenia kompletności).

## Findings (per kąt)

### A — Feasibility + seam (confidence: high, kodowy)
- Seam = nowy orkiestrator `summarize/sectioned.rs::summarize_sectioned(...) -> Result<(String, CallMeta)>` **nad** traitem (nie metoda traitu), router w `pipeline.rs:2279`: `egress_is_cloud(&connection) && connection=="anthropic"` → fan-out; inaczej obecna liniowa ścieżka.
- Firewall zachowany: N sekcji × `complete_with_meta`, każda redagowana + lease + ledger niezależnie.
- **🚨 Korektność prywatności:** Privacy Receipt bierze `redacted_pii` z JEDNEGO `call_meta` (`pipeline.rs:2157`) → orkiestrator **musi zwrócić `CallMeta.redactions` = SUMA** po N + froncie, inaczej receipt zaniża PII (RED-before-GREEN dla testu). Tokeny też sumować.
- `claude_code` spawnuje subprocess per-call (`claude_code.rs:825/877`), brak mutex-exclusion → N równoległych CLI strukturalnie dozwolone, ale N× runtime Node = RAM (historia OOM) + pułapka `has_unproven_process_group` (`perf.rs:616`) blokująca fail-closed cały kolejny egress jeśli jeden teardown padnie. **Rekomendacja: claude_code OFF albo cap=2.**
- Fan-in: **deterministyczny merge wystarczy w v1** (front-matter z jednej sekcji + reszta docina nagłówek+ciało; orkiestrator gwarantuje start od `---`). Synteza modelowa (dedup) = +1 wywołanie chmurowe → odłożyć.

### B — Czy to quality-win? (confidence: kierunek high, magnitudy med)
- Map-reduce bije single-pass na completeness/wierności **głównie gdy dokument przekracza okno** kontekstu; poniżej progu single-pass jest tańszy i spójniejszy. Nowoczesne modele (~200k–1M) przesuwają próg tak wysoko, że notatki ze spotkań **prawie nigdy** go nie przekraczają.
- Najmocniejszy realny argument ZA dekompozycją: **positional bias** — "Lost in the Middle" (Liu et al., TACL) + "positional bias of faithfulness" (Wan et al., 2410.23609): wierność spada dla treści ze **środka** długiego wejścia. Ale to bije tylko na DŁUGICH transkryptach.
- Meeting-specific (AMTSum, 2311.04292): zysk aspektowy bierze się z **retrievalu rozproszonych zdań aspektu**, NIE z N ślepych promptów na całości → ślepy fan-out nie odtwarza mechanizmu.
- **Tańsze alternatywy na kompletność bez N wywołań:** ustrukturyzowany prompt (nagłówek = dyrektywa "czego szukać") + **Chain-of-Density** (Adams et al., ACL 2023, dogęszczanie w jednym prompcie). Ostrzeżenie: wymuszony pure-JSON pogarsza małe modele (dotyczy `ollama`/`complete_json`).
- Fan-in to **realny podatek**: multi-doc łączenie podnosi halucynację/sprzeczność (2410.13961); ten sam fakt raz jako "decyzja" raz "action item"; per-speaker bez TL;DR halucynuje ramę → **per-speaker to najgorszy kandydat na niezależną sekcję**.

### C — Koszt/latencja + jak mierzyć (confidence: pricing med-high point-in-time)
- 1h ≈ ~11k tok wejścia, nota ~1,2k wyjścia, N=5, Opus 4.8 ($5/$25 za M). Baseline ~$0,085/notę; **naiwny fan-out ×5 ~$0,31 (~3,7×)**; warm-then-fanout z 5-min cache ~$0,13 (~1,5×) ale połowa latency-winu znika (trzeba zserializować pierwsze wywołanie).
- **Prompt caching nie ratuje prawdziwie równoległego fan-outu** (docs Anthropic: cache dostępny dopiero po starcie pierwszej odpowiedzi) — a `anthropic.rs:258-263` **dziś w ogóle nie ustawia `cache_control`** (tylko czyta `cache_read_input_tokens`), więc nawet warm-path wymaga przebudowy providera (transkrypt jako cacheowany prefix).
- `claude_code`: brak cache między spawnami; N× wypalanie Pro/Max rate-limitu + N Node procesów na RAM-ograniczonym Macu → może być **wolniej**.
- Blow-up jest **cały na wejściu** (zdublowany transkrypt); wyjście ~stałe.
- Lepiej-ukształtowany fan-out (gdyby kiedykolwiek) = **chunk map-reduce dla bardzo długich spotkań** (każde wywołanie widzi 1/N transkryptu, nie N× całość) — odwrotny, korzystny profil kosztu.
- **Egress transparency:** zostaw N wierszy w ledgerze (dokładność = własność bezpieczeństwa), ale **agreguj do wyświetlenia** — dowiąż `EgressEntry.meeting_id` (`egress_log.rs:41`, dziś `None`) + `call_kind="summarize_section"` żeby receipt zrolował N w jedną linię i **zsumował** redakcje.

### D — Multi-meeting jako lepszy cel + konkurencja (confidence: kod high, konkurencja med)
- Multi-meeting **nie jest greenfield** — shippuje jako single-pass, a ręczna
  ścieżka ma już bezstratne budżetowanie z jawnym pominięciem (#451). To nadal
  atrakcyjniejszy cel niż sekcyjny fan-out: per-meeting notatka **już jest
  "map"**, więc dodatkowy fan-out ma sens dopiero po zmierzeniu przepełnienia
  lub jakości reduce. Scheduler pozostaje osobnym miejscem do audytu.
- **Konkurencja (point-in-time 2026-07-25):** cross-meeting **chat** = table-stakes (mają wszyscy: Granola, Fireflies/AskFred, Otter, Fathom, tl;dv, Circleback, Fellow; my też via Ask). Generowany **raport** cross-meeting = *emerging* table-stakes (Otter Weekly Insights, tl;dv Multi-Meeting Reports, Circleback Friday→Slack); Granola/Fireflies/Fathom **nie** generują standalone raportu. → nasz weekly/topic report to parytet, **nie moat sam w sobie**; moat = **owned `.md` z `[[wikilinkami]]` do źródeł**.

## Fit z ograniczeniami Murmur

| Constraint | Sekcyjny fan-out notatki | Multi-meeting map-reduce | Lepszy single prompt |
|---|---|---|---|
| Local-first / egress (#1 "loud+justified") | ❌ N× egress na jedną notatkę, receipt regresuje | ⚠️ N× tylko dla przepełnienia; per-meeting map często = już-istniejąca notatka (0 nowego egress) | ✅ 1 egress |
| Obsidian owned `.md` | ✅ | ✅✅ (raport jako `.md` z `[[źródłami]]` = differentiator) | ✅ |
| SQLite-canonical | ✅ (1 scalona nota) | ✅ (derived export) | ✅ |
| Provider seam + redaction | ✅ (na `complete_with_meta`) | ✅ (reuse `provider_for(Role::Notes)`) | ✅ |
| macOS/RAM | ❌ `claude_code`×N = OOM | ⚠️ ale N małe (tylko overflow) | ✅ |
| CI honesty | merge assembler testowalny; jakość = ślepy odczyt na Macu | instrumentacja drop-rate testowalna; jakość = odczyt | metryki `notes_bakeoff` w `cargo test --lib` |

## Opcje i tradeoffy

- **Opcja 1 — Lepszy pojedynczy prompt (S, low risk).** Wzmocnić `template.rs`: jawne dyrektywy per-sekcja + chain-of-density krok "czy złapałeś każdy action item/decyzję?". 1 wywołanie, 1 egress, 0 podatku na spójność. **To baseline, który każdy fan-out musi pobić.** Natychmiast mierzalne przez `notes_bakeoff::compare`.
- **Opcja 2 — Eval-delta harness (S, low risk).** Rozszerzyć `notes_bakeoff.rs` o `LinearVsFanoutComparison` (NoteMetrics delta + action-item completeness via `action_items.rs` + section-redundancy score) + reuse `calibration.rs::coverage_fraction` (faithfulness) + `#[ignore]` Mac-runner generujący notę obiema drogami. **De-risk każdej dalszej decyzji danymi.**
- **Opcja 3 — Multi-meeting selektywny map-reduce (S→M).** Zostaw
  single-pass, gdy mieści się w budżecie; przy przepełnieniu fan-out per-meeting
  "map" (reuse istniejącej notatki jako mapy!) → reduce. Ręczny
  `generate_digest` ma już whole-note-or-skip i liczniki po #451. Pierwszy
  plaster powinien więc objąć scheduler
  `brief_runner::build_brief_corpus`: usunąć jego mid-note `take(remaining)` i
  dodać równoważne `meetings_included`/`meetings_omitted`.
- **Opcja 4 — Celowany 2. pass na Action Items + Decyzje (M).** Jedno dodatkowe wyspecjalizowane wywołanie (razem AI+decyzje, wspólny kontekst) rekoncyliujące notatkę. 2× egress (nie N×). Tylko jeśli eval pokaże spadek recall na długich.
- **Opcja 5 — Pełny 4-drożny sekcyjny fan-out + synteza (L, high risk).** ~4× egress/koszt, per-speaker halucynuje ramę, landmina redakcji. **Odłożyć/pominąć** — uzasadnione tylko gdy transkrypty regularnie wielogodzinne I latencja chmury boli (a note-gen jest w tle).

## Rekomendacja i pierwszy krok

**Nie budować sekcyjnego fan-outu (Opcja 5) teraz.** Zamiast tego, w kolejności wartość/koszt:

1. **Opcja 1 (lepszy prompt) — zrób teraz.** Największy quality-win na jednostkę wysiłku, zero podatku prywatności.
2. **Opcja 2 (eval harness) — równolegle.** Bez tego każda decyzja o fan-oucie jest na wiarę.
3. **Opcja 3 (multi-meeting), pierwszy krok = spike pomiarowy.** Ręczny
   `generate_digest` raportuje już nie-PII liczniki `included`/`omitted`.
   Ujednolicić scheduler z tym assemblerem albo dodać mu równoważne liczniki,
   następnie zmierzyć realne okno ≥80k. Jeśli pominięcia są częste,
   map-reduce jest uzasadniony; jeśli nie, lepszym użyciem silnika jest
   Opcja 1/4.

Bar weryfikacji jakości (czego `cargo test` nie udowodni): **ślepy side-by-side odczyt na realnym Macu, po polsku i angielsku**, + realny pomiar wall-clock i RSS `claude_code`×N przed jakimkolwiek włączeniem fan-outu.

## Otwarte pytania / czego nie zweryfikowano

- **Rozkład długości realnych spotkań usera** — cała decyzja B/C zależy od % spotkań przekraczających próg lost-in-the-middle; brak telemetrii. To decyduje między "lepszy prompt wystarcza" a "potrzebny map-reduce".
- **Realny omission-rate digestu** — ręczna ścieżka liczy pominięcia po #451,
  ale nie zmierzono ich na realnym vaulcie; scheduler nadal wymaga osobnej
  instrumentacji i testu.
- **Realny token-count 1h transkryptu** (~11k = estymata) — skaluje wszystkie liczby $; można przypiąć recepturą inspekcji dev-DB.
- **Koszt RAM N× `claude_code`** — oparty na komentarzach + historii OOM, nie zmierzony.
- **Czy jakikolwiek interaktywny surface potrzebuje sub-20s note-latency** — jeśli nie, nawet warm-then-fanout (anthropic) traci uzasadnienie. Decyzja produktowa.
- Konkretne magnitudy z literatury (AMTSum ROUGE, positional-faithfulness liczby) — kierunek potwierdzony, liczb nie wyekstrahowano z PDF; krążące ROUGE 0.3793 itp. = niezweryfikowane (nie było na pobranej stronie).

## Sources

**Kod (ten repo):** `pipeline.rs:2214-2279` (single-call note-gen) · `summarize/template.rs:10` (wielosekcyjny prompt) · `summarize/provider.rs:66-158` (trait/seam) · `summarize/mod.rs:68,193,349-361` (`egress_is_cloud`, consent-gate, `RedactingProvider` wrap) · `summarize/redact.rs:658,872-937` (per-call redakcja/lease/ledger + suma) · `summarize/anthropic.rs:258-263,117,172` (brak `cache_control`; czyta cache stats) · `summarize/claude_code.rs:669-676,825,877` (hermetyczny spawn per-call) · `summarize/action_items.rs:14` (parsowane, nie generowane) · `summarize/timeline.rs:193` + `commands/mod.rs:5422` (istniejący fan-out seam) · `commands/brief.rs:31,50-87` (manual digest: całe notatki albo pominięcie + liczniki po #451) · `brief_runner.rs:41-129` (automatyczny digest single-pass + mid-note `take(remaining)` truncation) · `summarize/digest.rs:10` (`[[Title]]` cytaty) · `summarize/threads.rs:19` (topic threads, no-LLM) · `commands/ask.rs:45,140,413` + `summarize/temporal.rs` (Ask-over-vault) · `state.rs:374-385` (`Semaphore(1)`) · `local.rs:234` (dowód serializacji) · `perf.rs:613-621` (egress lease, nie mutual-exclusion) · `egress_log.rs:22-43,137` + `storage/egress_store.rs:106` (ledger, `meeting_id` unwired) · `eval/notes_bakeoff.rs:40,101` + `eval/calibration.rs::coverage_fraction`.

**Web (point-in-time 2026-07-25):**
- Liu et al., *Lost in the Middle*, TACL — https://aclanthology.org/2024.tacl-1.9/
- Wan et al., *On Positional Bias of Faithfulness for Long-form Summarization* (2410.23609) — https://arxiv.org/pdf/2410.23609
- AMTSum, *Aspect-based Meeting Transcript Summarization* (2311.04292) — https://arxiv.org/abs/2311.04292
- Adams et al., *Chain of Density* (ACL 2023) — https://aclanthology.org/2023.newsum-1.7/
- *From Single to Multi: How LLMs Hallucinate in Multi-Document Summarization* (2410.13961) — https://arxiv.org/html/2410.13961v1
- *Why Multi-Agent LLM Systems Fail* (orq.ai) — https://orq.ai/blog/why-do-multi-agent-llm-systems-fail
- FutureAGI, *RAG Summarization 2026* — https://futureagi.com/blog/rag-summarization/
- AWS, *Meeting summarization with Amazon Nova* — https://aws.amazon.com/blogs/machine-learning/meeting-summarization-and-action-item-extraction-with-amazon-nova/
- Anthropic prompt-caching docs — https://platform.claude.com/docs/en/build-with-claude/prompt-caching
- Anthropic pricing — https://platform.claude.com/docs/en/about-claude/pricing · CloudZero — https://www.cloudzero.com/blog/claude-api-pricing/
- Claude Code limits — https://www.truefoundry.com/blog/claude-code-limits-explained · https://www.morphllm.com/claude-code-usage-limits
- Granola chat — https://www.granola.ai/blog/chat-with-meetings-search-analyze-ai-2026 · Otter Weekly Insights — https://otter.ai/blog/otter-weekly-insights · tl;dv review — https://www.meetjamie.ai/blog/tldv-review · Circleback — https://circleback.ai/blog/best-ai-meeting-assistants · Fireflies AskFred — https://docs.fireflies.ai/askfred/overview
