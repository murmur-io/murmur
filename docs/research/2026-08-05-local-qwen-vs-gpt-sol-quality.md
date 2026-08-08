# Local Qwen vs GPT-5.6-Sol: jakość na ścieżkach Murmura

Pierwotny pomiar: 2026-08-05. Source-bound refresh po historii Ask Brain: 2026-08-08. Bieżące
bootstrap runy to `ask-brain-chat-history-final-r1` (`2026-08-08T18:09:13.734113+00:00`) oraz
`ask-brain-chat-history-final-r2` (`2026-08-08T18:22:20.559772+00:00`).

Zakres: summary, Brain w notatce/meeting chat, popup na zaznaczonym tekście, Ask Brain po vault,
Brain w bieżącym spotkaniu, live bullets, trwała ekstrakcja faktów oraz retrieval Ask.

Referencja: GPT-5.6-Sol z żądanym `model_reasoning_effort="high"`. Protokół Codex CLI nie
atestuje modelu faktycznie obsłużonego ani efektywnego effortu.

## Decyzja w skrócie

Po remediacji zaimplementowanej w kandydackim worktree lokalne **candidate product paths** są
blisko referencji na tej małej, syntetycznej kohorcie: `28/34 = 82.4%` zaliczonych, wspólnych
obserwacji wobec `30/34 = 88.2%` dla Sol. Luka to
dwie odpowiedzi (`5.8 p.p.`); macro po surface wynosi `83.3%` local i `86.1%` Sol.

Nie oznacza to parytetu modeli. W osobnej próbie z tym samym evaluator-owned system/user envelope i
jednym zewnętrznym wywołaniem providera lokalny composite ma `26/36 = 72.2%`, a Sol `32/36 =
88.9%`. Luka jednowywołaniowego same-envelope model-stack wynosi więc `16.7 p.p.`, macro `66.7%`
vs `90.5%`, a błędów krytycznych jest `10` vs `4`. Produkcyjne prompty, parsery, routing,
stan i bounded orchestration
kompensują dużą część słabości małych modeli lokalnych.

Najważniejsza zmiana kandydacka jest poparta bezpośrednim lokalnym A/B: trwała post-call ekstrakcja
faktów w posturze **Fully Local** używa teraz klasy Heavy (obecnie Qwen 4B), nie Light (Qwen 1.7B). Na tych
samych dwóch przypadkach PL/EN i dwóch repetycjach fakty przeszły z `0/4` do `4/4`. Matched local
product wzrósł z `70.6%` do `82.4%`, a błędy krytyczne spadły z `10` do `6`. Hybrid pozostaje na
Light, żeby nie wymusić po cichu drugiego modelu, dodatkowego pobrania i współrezydencji RAM.

Rekomendacja:

- pozostawić 1.7B dla pracy live oraz 4B dla Notes/Ask i trwałych faktów w Fully Local;
- zachować wdrożone prompty, parsery i guardrail Ask;
- nie zmieniać jeszcze GGUF ani kwantyzacji;
- następny eksperyment wykonać na świeżej, większej kohorcie, szczególnie dla Ask, summary i popup;
- nie obiecywać „cloud parity”: produkt zbliżył się do Sol, ale sam lokalny model-stack nadal jest
  wyraźnie słabszy.

## Dwie oddzielne odpowiedzi na dwa różne pytania

| Lane | Co porównuje | Local | Sol | Luka Sol-local |
| --- | --- | ---: | ---: | ---: |
| candidate product system, wspólne przypadki | rzeczywiste role, prompty, parsery, routing i orchestration z tego worktree | 82.4% (28/34) | 88.2% (30/34) | 5.8 p.p. |
| same caller envelope/model-stack | jeden `complete_with_meta`, wspólny envelope i wspólna projekcja | 72.2% (26/36) | 88.9% (32/36) | 16.7 p.p. |

Pierwszy wiersz odpowiada na pytanie „jak candidate backend product paths zachowały się na tych
syntetycznych probes?”. Nie jest pełnym testem aplikacji/UI ani realnego vaultu. Drugi pokazuje, ile
przewagi Sol pozostaje, kiedy zdejmujemy większość produktowych podpórek. Nie wolno mieszać tych
liczb ani przedstawiać pierwszej jako benchmarku samych wag.

Granica równości drugiego lane'u jest precyzyjna: identyczne bajty system/user istnieją w callerze
evaluatora, a na obserwację przypada jedno zewnętrzne wywołanie `SummarizerProvider`. Nie atestujemy
identycznych promptów po renderowaniu adaptera, wewnętrznych retry providera ani identycznego
efektywnego inputu modelu (`providerRenderedPromptsByteIdentical=false`,
`effectiveModelInputsAttestedIdentical=false`).

## Wyniki candidate product paths

Dwa runy w odwróconej kolejności ramion dały 70 rekordów `product_path`: 68 rekordów tworzy 34
matched pary local-vs-Sol, a dwa pozostałe to lokalne live-bullets. Surowe archiwa zachowują też dwa
rekordy Sol live-bullets oznaczone wyłącznie jako offline ceiling; nie są product path ani częścią
matched wyniku.

| Kandydat / zakres | N | casePass | surface macro | Krytyczne | score |
| --- | ---: | ---: | ---: | ---: | ---: |
| Qwen 4B, przypisane product paths | 30 | 80.0% | 80.0% | 6 | 89.8 |
| Qwen 1.7B, live current + bullets | 6 | 100.0% | 100.0% | 0 | 100.0 |
| Local composite, wszystkie lokalne ścieżki | 36 | 83.3% | 85.7% | 6 | 91.5 |
| Local composite, tylko wspólne z Sol | 34 | 82.4% | 83.3% | 6 | 91.0 |
| Sol, te same 34 product paths | 34 | 88.2% | 86.1% | 4 | 93.9 |

| Surface | Local candidate route | Sol candidate/reference route | Odczyt |
| --- | ---: | ---: | --- |
| Summary | Qwen 4B: 66.7% (4/6) | 66.7% (4/6) | oba mają krytyczne błędy; brak parytetu ogólnego |
| Brain w meeting/note chat | Qwen 4B: 100% (4/4) | 100% (4/4) | pozytywny, lecz bardzo mały test |
| Popup zaznaczonego tekstu | Qwen 4B: 83.3% (10/12) | 100% (12/12) | nadal materialna luka lokalna |
| Ask Brain / vault | Qwen 4B: 50% (2/4) | 50% (2/4) | local stabilnie przechodzi EN i oblewa PL; Sol zamienia verdict między przypadkami w R1/R2 |
| Trwałe fakty post-call | Qwen 4B: 100% (4/4) | 100% (4/4) | routing naprawił zmierzoną klasę błędu |
| Brain live current | Qwen 1.7B: 100% (4/4) | 100% (4/4) | utrzymać 1.7B, ale powiększyć próbkę |
| Live bullets | Qwen 1.7B: 100% (2/2) | nieporównywalny product route | Sol jest tylko offline ceiling |

`callSuccessRate=100%` oznacza brak błędu procesu providera. Nie oznacza poprawnej odpowiedzi,
dobrego retrieval ani niezawodności produktu.

### Błędy końcowe product route i stabilność

- Qwen 4B, `ask-vault-pl-orchid`: fail w R1/R2; nie przechodzi `forbiddenPass` ani
  `relationPass`.
- Qwen 4B, `note-popup-actions-pl`: fail w R1/R2 na `relationPass`.
- Qwen 4B, `summary-en-cedar`: fail w R1/R2 na `forbiddenPass` i `sectionPass`.
- Sol, `summary-pl-kestrel`: fail w R1/R2 na `forbiddenPass`, `relationPass` i `sectionPass`.
- Sol, `ask-vault-en-quartz-holdout`: fail w R1, pass w R2; w R1 nie przechodzi
  `provenancePass`, `relationPass` ani `toolPolicyPass`.
- Sol, `ask-vault-pl-orchid`: pass w R1, fail w R2; w R2 nie przechodzi `languagePass`,
  `provenancePass`, `relationPass` ani `toolPolicyPass`.

Wszystkie finalne lokalne outcome'y, w tym przypadki inne niż fakty, oraz ich `outputSha256` były
stabilne między R1 i R2. Cloud nie był deterministyczny: zmienił finalny output w 10 z 18 rekordów
route-specific, czyli w 9 z 17 product paths oraz w offline live-bullets ceiling; dwa przypadki Ask
zmieniły również pass/critical verdict. W same-envelope lane Sol zmienił 8 z 18 outputów bez zmiany
verdictów, a oba ramiona lokalne pozostały byte-stable.

## Wyniki same caller envelope/model-stack

| Para | Local casePass / macro / krytyczne / score | Sol casePass / macro / krytyczne | Luka score Sol-local |
| --- | ---: | ---: | ---: |
| Qwen 4B vs Sol, N=30 | 73.3% / 73.3% / 8 / 86.4 | 86.7% / 86.7% / 4 | 6.8 |
| Qwen 1.7B vs Sol, N=6 | 66.7% / 50.0% / 2 / 83.0 | 100% / 100% / 0 | 17.0 |
| Local composite vs Sol, N=36 | 72.2% / 66.7% / 10 / 85.8 | 88.9% / 90.5% / 4 | 8.5 |

| Surface | Local same-envelope | Sol same-envelope |
| --- | ---: | ---: |
| Summary | Qwen 4B: 33.3% (2/6), critical 4, score 66.0 | 33.3% (2/6), critical 4, score 66.0 |
| Meeting chat | Qwen 4B: 100% (4/4) | 100% (4/4) |
| Popup/note assist | Qwen 4B: 83.3% (10/12), critical 2, score 91.5 | 100% (12/12), score 100.0 |
| Ask Brain | Qwen 4B: 50% (2/4), critical 2, score 74.5 | 100% (4/4), score 100.0 |
| Trwałe fakty | Qwen 4B: 100% (4/4) | 100% (4/4) |
| Live current | Qwen 1.7B: 100% (4/4) | 100% (4/4) |
| Live bullets | Qwen 1.7B: 0% (0/2), critical 2, score 49.0 | 100% (2/2), score 100.0 |

Produktowa ścieżka live-bullets 1.7B przechodzi 2/2, podczas gdy ten sam surface w
jednowywołaniowym same-envelope lane przechodzi 0/2 i w obu repetycjach oblewa
`forbiddenPass`. To pokazuje, dlaczego candidate product path może wypaść dużo lepiej niż
jednowywołaniowy same-envelope model-stack.

## Efekt remediacji

### 1. Routing faktów: bezpośrednie, powtórzone A/B ramion lokalnych

| Stan | Matched local product | Sol | Luka score | Krytyczne local |
| --- | ---: | ---: | ---: | ---: |
| przed zmianą, fakty na 1.7B | 70.6% (24/34) | 85.3% (29/34) | 7.4 | 10 |
| final, fakty na 4B w Fully Local | 82.4% (28/34) | 88.2% (30/34) | 2.9 | 6 |

W A/B lokalnym wszystkie cztery obserwacje faktów zmieniły się z fail na pass, a 32 obserwacje
pozostałych lokalnych product paths (16 przypadków razy dwie repetycje) zachowały identyczne
`outputSha256` i outcome. Zmianą zachowania lokalnego był klasowy routing trwałych faktów i zgodne
z nim oznaczenie eval route/profile; osobno skorygowano bookkeeping oczekiwanej liczby wywołań w
validatorze, bez zmiany lokalnych outputów ani score'a.

Sol nie jest zamrożonym ramieniem kontrolnym: między decision point i kolejnym source-bound
refreshem zmieniał outputy, a agregat przesunął się z `29/34` do `30/34`. Dlatego
bezpośredni wniosek przyczynowy dotyczy poprawy local `24/34 -> 28/34`, nie stabilności cloud.
Historyczny punkt pre-routing nie ma też osobnego evidence manifestu/replay binding, więc jego
atrybucja jest słabiej związana niż wynik finalny. Routing wybiera `ModelClass::Heavy`, nie
hardkodowane ID Qwena; wybrany przez użytkownika model Heavy nadal jest respektowany. Brak modelu
degraduje do lokalnego stuba, nigdy do chmury.

### 2. Wcześniejszy pakiet promptów i guardraili: wynik kierunkowy

Pierwotne, zachowane outputy Qwen 4B po przeliczeniu finalnym oracle'em miały `14/26 = 53.8%` i 12
błędów krytycznych. Final na tych 13 przypadkach ma `20/26 = 76.9%` i 6 błędów krytycznych;
summary wzrosło z `0/6` do `4/6`, popup z `8/12` do `10/12`.

To before/after całego pakietu, nie izolowany eksperyment jednego promptu: zmieniły się prompty,
assembly, Ask guardrail, adjudykowany scorer i transport effortu referencji. Nie jest to dowód
generalizacji. Zachowane outputy baseline są po to, aby zmiana oracle'a nie udawała zmiany generacji.

## Co zaimplementowano w kandydacie

- Summary dostało inventory decyzji/zobowiązań przed pisaniem, ochronę zakresu, modalności, ownera
  i terminu oraz stabilne polskie sekcje `## Decyzje` / `## Zadania`.
- Assembly odzyskuje wyłącznie wąski przypadek niedomkniętego YAML przed pierwszym nagłówkiem; nie
  rozluźnia canonical storage splittera ani nie ucina treści po poziomej linii.
- Popup action/fact-check wymaga dokładnego podmiotu, zakresu, lokalizacji i modalności oraz
  rozróżnia zobowiązanie przyszłe od czynności wykonanej.
- Ask/chat wymaga odpowiedzi osobno na sub-pytania i zachowania współrzędnych faktu. Agent Ask
  odrzuca odpowiedź „unknown/nie mogę zweryfikować” po pozytywnym search bez otwarcia pasującego
  źródła i ponawia próbę w ramach istniejącego, sześciostopniowego budżetu. Trace jest prywatny
  i content-free.
- Codex CLI przekazuje wyłącznie allowlistowany `model_reasoning_effort`; benchmark żąda `high`.
- Fully Local kieruje trwałą post-call ekstrakcję faktów do Heavy; Hybrid i ścieżki podczas nagrania
  zachowują Light. Nie ma local-to-cloud fallbacku ani ukrytego auto-downloadu.
- Dodano powtarzalny eval generation/retrieval, raw structured observation wyłącznie dla testów,
  deterministyczne replay oracle'y, źródłowe fingerprinty i trwały SQLCipher egress evidence sink.

## Retrieval Ask Brain

Oddzielny lane uruchomił rzeczywisty `multilingual-e5-small`, FTS5, semantic search i obecną
product-code hybrid fusion na tymczasowym SQLCipher DB. Czytania używały visible-only metod przy pustym zestawie
session unlock; DB zostało posprzątane.

| Retrieval, k=5 | Recall@k | nDCG@k | MRR |
| --- | ---: | ---: | ---: |
| FTS product | 0.550 | 0.550 | 0.550 |
| Semantic product floor | 0.767 | 0.659 | 0.640 |
| Hybrid product | 0.900 | 0.791 | 0.768 |

Hybrid recall wynosi `1.000` dla 7 zapytań EN i `0.846` dla 13 zapytań PL. To uzasadnia
utrzymanie hybrydy; nie uzasadnia claimu o jakości retrieval na realnych vaultach.

## Metodologia, privacy i provenance

Fixture ma 18 całkowicie syntetycznych przypadków PL/EN. Dwa runy wykonały ramiona w odwrotnej
kolejności (`4B -> 1.7B -> Sol` oraz `Sol -> 1.7B -> 4B`). Surowe archiwa zawierają 144 rekordy
generacji: 70 `product_path`, dwa Sol live-bullets `offline_reference_ceiling` oraz 72
same-envelope/model-stack. Główne dwie lane'y mają więc 142 obserwacje; dwa ceiling records są
zachowane, ale wyłączone z product comparison. Każdy verdict pochodzi z kodowego oracle'a; żaden
model nie oceniał odpowiedzi innego modelu. Raw report ma schema v9; `holdout` jest wyłącznie
historycznym tagiem sprzed remediacji, a nie nadal niewidzianą kohortą generalizacyjną.

Archiwa celowo zachowują w jawnej postaci oceniany output wygenerowany wyłącznie z tego zmyślonego
fixture'u. Product-path records przechowują także raw structured observation tam, gdzie evaluator
ją zbiera. Same-envelope records przechowują natomiast projekcję ocenianą przez oracle oraz tylko
`rawOutputSha256`/`rawOutputChars`, nie pełną surową odpowiedź providera. Nie zapisują promptów,
audio, zawartości użytkownika ani realnego vaultu. Osobny inventory publikuje i wiąże hashem każdą
rzeczywiście zachowaną tekstową wartość z obu finalnych runów, dzięki czemu ten warunek jest
audytowalny, a nie oparty na deklaracji.

Oracle sprawdza wymagane fakty, krytyczne daty/ownerów/liczby/statusy, relacje, negacje, zakazane
twierdzenia, format, język, provenance, staged tool policy, convergence i zastosowanie stanu.
Python validator sprawdza commitments outputów i rekordów, odwróconą kolejność, snapshot start=end,
runtime/model hashes oraz egress receipts, po czym przelicza agregaty i rekonstruuje cały combined
report. Osobny deterministyczny Rust replay ponownie wylicza każdy score z zachowanego, ocenianego
outputu; sam Python `--verify-evidence` nie potwierdza poprawności verdictów. W same-envelope lane
nie jest to niezależny replay transformacji raw-response -> projekcja, bo raw response ma tylko
commitment, co jawnie ogranicza siłę dowodu tego lane'u.

Cloud nadal przechodzi przez canonical consent/redaction/ledger seam. Tymczasowy SQLCipher ledger
zapisał `39/39` content-free receipts zarówno w R1, jak i R2, bez failure; oba runy mają po `36`
wpisów `complete` i `3` `summarize`. Następnie DB usunięto. Projekcja odnotowała po `8`
podstawień wzorca telefonu w każdym runie, w `6` wierszach z redakcją; redakcje
card/email/name pozostały zerowe. Same-envelope lane obejmuje 18 jednowywołaniowych cloud
observations na run. Projekcja publikuje tylko agregat redakcji dla wszystkich 39 cloud calls na
run, dlatego tych 8 podstawień nie przypisujemy osobnemu lane'owi.

Name NER był celowo ustawiony na deterministyczny `NoopNameRedactor`, ponieważ corpus jest
syntetyczny. Pomiar nie testuje więc skuteczności redakcji nazw własnych w trybie produkcyjnym.

Artefakty zawierają wyłącznie dane syntetyczne. Nie ma audio, realnych notatek, transkryptów,
promptów użytkownika, sekretów ani PII. Produkcyjny egress ledger pozostaje content-free.

### Uczciwe ograniczenia evidence

- mały, adjudykowany corpus mierzy znane kontrakty; po remediacji dawny `holdout` jest tylko legacy
  tagiem i nie jest już niewidzianym testem generalizacji;
- naturalność, styl i ogólna użyteczność prozy nie są oceniane; potrzebny jest zaślepiony panel
  ludzi, nie model-judge;
- hash i snapshot są self-declared, nie kryptograficzną atestacją pochodzenia;
- `sourceFingerprint` obejmuje skończoną allowlistę 67 plików, w tym
  `src/settings/postures.rs`; odczyt brakującej zależności failuje, a selftest wymaga braku
  duplikatów, istnienia plików, wymaganych zależności i identycznej listy Rust/Python. Nadal nie
  jest to automatyczny graf wszystkich tranzytywnych zależności;
- `--verify-evidence` sprawdza zapisany `trackedDiffSha256=e3b0c442…` i
  `workingTreeDirty=false`; pomiar powstał na czystym commicie. Validator celowo nie przelicza
  późniejszego bieżącego diffu Git, a pełny Harness wiąże osobno exact diff artefaktów i raportu;
- tool steps są content-free evidence sesyjnym, nie uwierzytelnionym transkryptem wywołań;
- pełny zewnętrzny log runnera nie jest commitowany ze względu na prywatne ścieżki i kontekst;
  dlatego historyczny status wyjścia wrappera nie jest częścią tego dowodu. Certyfikowana jest
  kompletność i odtwarzalność zachowanych raportów, nie provenance zakończenia procesu;
- same-envelope records nie zachowują plaintext raw response; wiążą je tylko przez
  `rawOutputSha256`/`rawOutputChars`, więc replay ponownie ocenia projekcję, ale nie dowodzi
  niezależnie poprawności transformacji raw-response -> projekcja;
- jeden outer provider call nie dowodzi braku retry wewnątrz providera; same-caller-envelope
  model-stack nie izoluje samych wag i nie atestuje parity efektywnego promptu po adapterze;
- Sol ma requested `gpt-5.6-sol/high`, lecz brak served-model i effective-effort attestation;
- finalna content-free review projection nie publikuje timingów, RAM, energii ani tokens/s; ten
  raport nie stawia więc wniosku wydajnościowego.

## Latencja

Surowe raporty zachowują per-case `durationMs`, ale końcowa, content-free review projection nie
publikuje odtwarzalnych agregatów czasu. Poprzednią tabelę usunięto, aby nie mieszać metryk z innej
repetycji ani warstwy evidence. Wydajność wymaga osobnego pomiaru cold/warm, RAM, energii i
tokens/s na docelowych Macach; ten benchmark rozstrzyga wyłącznie jakość.

## Następny plan jakości

1. Zamrozić nową kohortę Ask przed dalszym kodem: więcej pytań wieloslotowych PL/EN, brak faktu,
   sprzeczne statusy i relock między search/open.
2. Jeśli wynik się potwierdzi, dodać kodowy, maksymalnie czteroslotowy `AskSlotPlan`, osobny wynik
   i źródło per slot oraz jeden local-only rewrite przy niepełnym pokryciu. Nie dokładać kolejnej
   ogólnej instrukcji promptowej na obecnym corpusie.
3. Dla cloud Ask rozważyć code-owned `search -> open` prelude lub odrzucenie odpowiedzi z zerowym
   odczytem. Każde czytanie nadal przez gated tools; żadnego cichego cloud fallbacku.
4. Dla summary/popup zamrozić świeże przypadki i porównać 4B z bounded `draft -> deterministic
   audit/rewrite`; warunkiem jest spadek błędów krytycznych bez nieakceptowalnego p95/RAM.
5. Dopiero potem porównywać większy lokalny model/inną kwantyzację, z tym samym harness-em oraz
   pomiarem RAM, cold startu i energii na docelowych Macach.

## Artefakty

Bieżący bootstrap przed powtórzeniem na commicie C1:

- R1 archive `c044a7f5cde805e9c949df45aac180d91840579a73f19e13aec2aa7f91164c3f`,
  logical JSON `beee7f543eb38e9283dcbea27ed16946e011d4b41e9e453120645679f22c1488`;
- R2 archive `640d3a775ff09e6efae88ab0bac727cddb82b8b1c25a334b60e474b468aa755b`,
  logical JSON `2344f13087c36ff059116f2d55e448502b50056e45d0bd65eaed84807ff959b0`;
- versioned combined `420d3cbb7495aba364e5d45c34261a7bd0d76f36e75fbdf37ac77ff126c69749`;
- jawny snapshot syntetycznego fixture
  `b5f63efbc135a8629366614444bdba8d9501e28209d054e967b8e9debeddd9b2`;
- jawny inventory wszystkich tekstowych wartości R1/R2: deterministyczne archiwum
  `54027418a91b6c47c53e357724cf424b13aa344e969032aaa824e786341c40fc`, logiczny JSON
  `00fa7c6af7ececadddd36a500b7ff1aa4179239e5e9fd9d9d616d47c7ea07258`;
- evidence manifest `68a933943a9bb942e276ec2d3eb1e5a9b916f1074b1bc9100ea1bd3e85d41fe4`;
- tekstowa review projection
  `add2b40cabbc76fd87b67f623cf614f60cc2d93e77090aad55ea50899170caac`.

W evidence `logicalPath` jest wirtualną nazwą zdekompresowanego JSON, nie drugim plikiem w repo.
Ścisły evidence manifest schema v1 wiąże fizyczne archiwa `.json.gz`, ich logiczną treść, combined
oraz producer snapshot. Celowo nie ma w nim dodatkowych kluczy snapshot/inventory, bo source-bound
repeat validator odrzuca zmianę tego schematu. Te dwa są wiązane osobno przez stałe ścieżki i SHA w
`verify_local_cloud_quality_artifacts.py`; ten oracle jest uruchamiany przez test Rust razem z
mutation selftestami obu plików. Snapshot pozwala odtworzyć każdy `casePayloadSha256`, a inventory
publikuje wszystkie 724 unikalne stringi z 4 751 wystąpień wraz z commitmentami ścieżek per
repeat. Raw reports mają schema v9, combined schema v5, inventory schema v2.
Odświeżone R1/R2 oraz inventory mają nowe, wersjonowane ścieżki; starsze pliki z bazy pozostają
historycznymi wejściami i nie są po cichu nadpisywane. R1/R2 i inventory są deterministycznymi,
jednoczłonowymi archiwami gzip bez nazwy pliku i z `mtime=0`; combined pozostaje zwykłym UTF-8
JSON-em dla niezależnego replayu source-bound validatora. Nie ma lokalnej reguły `.gitattributes`,
więc hash exact diffu nie zależy od tego, czy Git liczy go z checkoutu bazy, czy kandydata.
Sąsiadująca, kompaktowa tekstowa review projection publikuje load-bearing bindingi, agregaty, failure'y,
flipy, stabilność, retrieval, egress/redakcje i commitment inventory bez plaintext outputów. Oracle
odtwarza ją dokładnie z R1/R2, combined, evidence i inventory, porównuje kanoniczne bajty oraz
odrzuca ponownie zahashowaną zmianę semantyczną; nadal otwiera i waliduje pełną logiczną treść.

Bieżące bootstrap runy wiążą clean snapshot commit `d672583e3181a33631b6930b28feef0a2fdacf2f`, source fingerprint
`3201e12357f49442a259e131ba27192316def7cf7980c79f8006258bbfdad442`, manifest
`21ea3cc236b8c4058f18043b538bac87e93e933c86bd8b0c3696f5b67d45f01d`, evaluator
`41e828e449382fb9df672b20ebd833d962b68aa3455e671477ff936bd20a89f9`, fixture
`b5f63efbc135a8629366614444bdba8d9501e28209d054e967b8e9debeddd9b2` i validator
`2ebe73a29054f68a3c93a3682778f0ff32ec2d40e3ce8c6a0e156be9379a1431`.

Runtime local to `murmur-brain-workspace-build`, SHA-256
`1fa8425a068784b4659bafe48fed6cb7b737902dfeb85e04de4a2b220f0758ab`; runtime cloud to
`codex-cli 0.146.0`, SHA-256
`ae1d3ffe6d48aec6a4dc3f50e7eb8e0d11962485a6a9406c5a7012139383da02`. Produkcyjne proofy
izolacji są celowo przypięte do linii `0.146.x`; znalezione `0.147.0` failuje zamknięcie zamiast
być po cichu dopuszczone bez ponownej weryfikacji kontraktu. Wagi 4B miały
2,497,280,736 B i SHA-256 `2fde00ce69dd4899c70d020845e2638353015bba0fdf161b3eb965f2bca4464e`;
wagi 1.7B miały 1,282,439,584 B i SHA-256
`72c5c3cb38fa32d5256e2fe30d03e7a64c6c79e668ad84057e3bd66e250b24fb`.

Pre-routing decision point:

- bezstratny bundle dwóch raw JSON-ów `eval/results/2026-08-05-qwen-vs-gpt-sol-decision-raw.json.xz`
  ma SHA-256
  `18521020093fe4c3f1d39027e319fd4e9dea8ee9758464c48b7e0bb81349a5ed`;
- wewnętrzny manifest zachowuje hashe obu pierwotnych archiwów, logicznych JSON-ów i pochodnego
  combined `a024c257ae79939a61879c54db02c3f845d2c46286b567fff443d925f00c72d5`.

Pierwotne baseline R1/R2 są bezstratnie zapisane w
`eval/results/2026-08-05-qwen-vs-gpt-sol-baseline-raw.json.xz` o SHA-256
`05f248da2a6e1104b9311d2fd21422c9dc2af10c1cf0492472812295509f2186`. Jego wewnętrzny
manifest zachowuje hashe oryginalnych archiwów/logicznych JSON-ów i pochodnych final-oracle
rescore. Oba historyczne bundle są kompaktowymi JSON-ami z dokładnymi pierwotnymi bajtami gzip
zakodowanymi jako base64, skompresowanymi deterministycznym XZ/LZMA2 preset 9; można je odczytać
standardowym modułem Python `lzma`. Nie są używane jako
bezpośredni A/B dla Sol, bo pierwotny run nie żądał `high`.

## Stan weryfikacji

Po catch-upie do trunku zachowano dwa kompletne bootstrapowe real-model raporty związane z clean
snapshotem `d672583e3181a33631b6930b28feef0a2fdacf2f`. Nie atestują jeszcze bieżącego exact diffu
ani przyszłego C1. Każdy zawiera wszystkie oczekiwane ramiona i przypadki, a ich kolejność jest
odwrócona między R1 i R2. Python validator przyjął oba artefakty, odtworzył combined i zweryfikował
runtime/source binding oraz `workingTreeDirty=false` dla obu snapshotów. Commitowany dowód celowo
nie stawia claimu o historycznym kodzie wyjścia zewnętrznego wrappera; kompletność wynika z
zamkniętych schematów, hashy, pełnego inventory i niezależnego replayu wyników.
Deterministyczny replay jest częścią offline oracle'a. Pełne bramki projektu i dokładny werdykt
Harnessu zostaną raportowane w PR/receipt dopiero po ich zakończeniu; ten raport sam ich nie
atestuje i nie należy mylić uruchomienia modeli z końcowym PASS zmiany. Po commicie C1 oba runy
zostaną wykonane ponownie na jego czystym SHA, a ta sekcja i wszystkie bindingi zostaną zastąpione
finalnym evidence w osobnej, exact-diff warstwie C2.
