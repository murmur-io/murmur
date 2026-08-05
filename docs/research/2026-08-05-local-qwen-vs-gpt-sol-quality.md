# Local Qwen vs GPT-5.6-Sol: jakość na ścieżkach Murmura

Data pomiaru: 2026-08-05

Zakres: summary, Brain w notatce/meeting chat, popup na zaznaczonym tekście, Ask Brain po vault,
Brain w bieżącym spotkaniu, live bullets, trwała ekstrakcja faktów oraz retrieval Ask.

Referencja: GPT-5.6-Sol z żądanym `model_reasoning_effort="high"`. Protokół Codex CLI nie
atestuje modelu faktycznie obsłużonego ani efektywnego effortu.

## Decyzja w skrócie

Po remediacji zaimplementowanej w kandydackim worktree lokalne **candidate product paths** są
blisko referencji na tej małej, syntetycznej kohorcie: `28/34 = 82.4%` zaliczonych, wspólnych
obserwacji wobec `29/34 = 85.3%` dla Sol. Luka to
jedna odpowiedź (`2.9 p.p.`); macro po surface wynosi `83.3%` local i `82.0%` Sol.

Nie oznacza to parytetu modeli. W osobnej próbie z tym samym evaluator-owned system/user envelope i
jednym zewnętrznym wywołaniem providera lokalny composite ma `26/36 = 72.2%`, a Sol `33/36 =
91.7%`. Luka jednowywołaniowego same-envelope model-stack wynosi więc `19.5 p.p.`, macro `66.7%`
vs `91.7%`, a błędów krytycznych jest `10` vs `3`. Produkcyjne prompty, parsery, routing,
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
| candidate product system, wspólne przypadki | rzeczywiste role, prompty, parsery, routing i orchestration z tego worktree | 82.4% (28/34) | 85.3% (29/34) | 2.9 p.p. |
| same caller envelope/model-stack | jeden `complete_with_meta`, wspólny envelope i wspólna projekcja | 72.2% (26/36) | 91.7% (33/36) | 19.5 p.p. |

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
| Sol, te same 34 product paths | 34 | 85.3% | 82.0% | 5 | 92.5 |

| Surface | Local candidate route | Sol candidate/reference route | Odczyt |
| --- | ---: | ---: | --- |
| Summary | Qwen 4B: 66.7% (4/6) | 66.7% (4/6) | oba mają krytyczne błędy; brak parytetu ogólnego |
| Brain w meeting/note chat | Qwen 4B: 100% (4/4) | 100% (4/4) | pozytywny, lecz bardzo mały test |
| Popup zaznaczonego tekstu | Qwen 4B: 83.3% (10/12) | 100% (12/12) | nadal materialna luka lokalna |
| Ask Brain / vault | Qwen 4B: 50% (2/4) | 25% (1/4) | Sol zalicza jeden z czterech runów i ma jeden flip |
| Trwałe fakty post-call | Qwen 4B: 100% (4/4) | 100% (4/4) | routing naprawił zmierzoną klasę błędu |
| Brain live current | Qwen 1.7B: 100% (4/4) | 100% (4/4) | utrzymać 1.7B, ale powiększyć próbkę |
| Live bullets | Qwen 1.7B: 100% (2/2) | nieporównywalny product route | Sol jest tylko offline ceiling |

`callSuccessRate=100%` oznacza brak błędu procesu providera. Nie oznacza poprawnej odpowiedzi,
dobrego retrieval ani niezawodności produktu.

### Stabilne błędy końcowe

- Qwen 4B, `ask-vault-pl-orchid`: miesza otwarty budżet z zatwierdzonym startem i pomija datę.
- Qwen 4B, `note-popup-actions-pl`: skraca relację `plan testów` do `plan`.
- Qwen 4B, `summary-en-cedar`: opisuje zatwierdzony rollout w prozie, ale deklaruje brak decyzji.
- Sol, `ask-vault-en-quartz-holdout`: w obu product-path repetycjach kończy bez narzędzi; nie
  dostarcza wymaganych faktów, provenance ani staged search/open.
- Sol, `summary-pl-kestrel`: w obu repetycjach miesza zatwierdzony limit budżetu z planowanym
  terminem startu pilotażu.
- Sol, `ask-vault-pl-orchid`: nie przechodzi w R1 i przechodzi w R2; to jedyny flip `casePass` w
  finalnym product lane. Wszystkie finalne lokalne outcome'y, w tym przypadki inne niż fakty, oraz
  ich `outputSha256` były stabilne między R1 i R2. Cloud nie był deterministyczny: zmienił output
  w 7 z 18 przypadków między repetycjami.

## Wyniki same caller envelope/model-stack

| Kandydat | N | casePass | surface macro | Krytyczne | score |
| --- | ---: | ---: | ---: | ---: | ---: |
| Qwen 4B | 30 | 73.3% | 73.3% | 8 | 86.4 |
| Sol na tych samych przypadkach | 30 | 90.0% | 88.3% | 3 | 94.9 |
| Qwen 1.7B | 6 | 66.7% | 50.0% | 2 | 83.0 |
| Sol na tych samych przypadkach | 6 | 100% | 100% | 0 | 100.0 |
| Local composite | 36 | 72.2% | 66.7% | 10 | 85.8 |
| Sol | 36 | 91.7% | 91.7% | 3 | 95.8 |

Produktowa ścieżka live-bullets 1.7B przechodzi dzięki parserowi i regułom stanu, podczas gdy
jednowywołaniowa projekcja model-only nie utrzymuje poprawnie zakresu i powtarza wcześniejszy fakt.
To dobry przykład, dlaczego candidate product path może wypaść dużo lepiej niż jednowywołaniowy
same-envelope model-stack.

## Efekt remediacji

### 1. Routing faktów: bezpośrednie, powtórzone A/B ramion lokalnych

| Stan | Matched local product | Sol | Luka score | Krytyczne local |
| --- | ---: | ---: | ---: | ---: |
| przed zmianą, fakty na 1.7B | 70.6% (24/34) | 85.3% (29/34) | 7.4 | 10 |
| final, fakty na 4B w Fully Local | 82.4% (28/34) | 85.3% (29/34) | 1.5 | 6 |

W A/B lokalnym wszystkie cztery obserwacje faktów zmieniły się z fail na pass, a 32 obserwacje
pozostałych lokalnych product paths (16 przypadków razy dwie repetycje) zachowały identyczne
`outputSha256` i outcome. Zmianą zachowania lokalnego był klasowy routing trwałych faktów i zgodne
z nim oznaczenie eval route/profile; osobno skorygowano bookkeeping oczekiwanej liczby wywołań w
validatorze, bez zmiany lokalnych outputów ani score'a.

Sol nie jest zamrożonym ramieniem kontrolnym: między decision point i finalnym rerunem zmieniał
outputy oraz pojedyncze outcome'y, choć zagregowany `29/34` przypadkiem pozostał taki sam. Dlatego
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
zapisał `36/36` content-free receipts w R1 i `40/40` w R2, bez failure; różnica pochodzi z liczby
kroków cloud agent loop, nie z brakujących rekordów. Następnie DB usunięto. Product route
odnotował 5 podstawień wzorca telefonu w R1 i 9 w R2 (kontrolowane daty/ranges w fixture); nie
osłabiono regexu ani nie ominięto firewalla. Same-envelope lane używa odwracalnych,
kandydat-independent tokenów semantycznych przed canonical firewall; wszystkie 18 cloud receipts
na run miało zero redakcji, więc Sol nie dostał dat oznaczonych jako `PHONE`.

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
- `--verify-evidence` sprawdza zapisany `trackedDiffSha256=2abc82db…`, lecz celowo nie przelicza go
  z bieżącego Git; snapshot był brudnym worktree przy pomiarze, a pełny Harness wiąże osobno exact
  diff. Sam manifest nie dowodzi, że późniejszy checkout ma ten sam diff;
- tool steps są content-free evidence sesyjnym, nie uwierzytelnionym transkryptem wywołań;
- same-envelope records nie zachowują plaintext raw response; wiążą je tylko przez
  `rawOutputSha256`/`rawOutputChars`, więc replay ponownie ocenia projekcję, ale nie dowodzi
  niezależnie poprawności transformacji raw-response -> projekcja;
- jeden outer provider call nie dowodzi braku retry wewnątrz providera; same-caller-envelope
  model-stack nie izoluje samych wag i nie atestuje parity efektywnego promptu po adapterze;
- Sol ma requested `gpt-5.6-sol/high`, lecz brak served-model i effective-effort attestation;
- latency jest warm/resident na jednym Macu, bez cold-startu, RAM, energii i tokens/s.

## Latencja pomocnicza

Mac16,5, Apple M4 Max, 64 GiB, macOS 26.5 (build 25F71); dwie repetycje, pełny czas
product-path przypadku. `p50` i `p95` poniżej to nearest-rank po połączonych repetycjach.

| Ramię | N | mean | p50 | p95 | min-max |
| --- | ---: | ---: | ---: | ---: | ---: |
| Qwen 4B | 30 | 5.96 s | 3.74 s | 20.11 s | 0.75-23.31 s |
| Qwen 1.7B | 6 | 1.75 s | 1.48 s | 2.62 s | 1.02-2.62 s |
| Sol, requested high | 34 | 7.53 s | 5.43 s | 14.26 s | 3.31-47.69 s |

To diagnostyka, nie SLA. Sidecar był rezydentny w ramach ramienia, a próbka jest zbyt mała do
decyzji o wydajności lub termice.

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

Final:

- R1 archive `fc7c70eb07595b8ae768a9a53d2d8e6d12544b086a5d4173540acb873618fb2f`,
  logical JSON `3308ec93608ca21a190e6bf14f0082d98f8cde545e1c9f8fe017f9c2f07bac51`;
- R2 archive `0142628e94dd3756cbad3d2d067def02b19b830e95107c7be82dbcc3344aa6cf`,
  logical JSON `393ddee23d7bdff7604a2333f0303d3fea8c398769e1732519d4bfc479973d64`;
- combined `6db53789cc979743ab6deaf74a0aba093c94df2825abef32ea93cb801effd3cc`;
- jawny snapshot syntetycznego fixture
  `b5f63efbc135a8629366614444bdba8d9501e28209d054e967b8e9debeddd9b2`;
- jawny inventory wszystkich tekstowych wartości R1/R2
  `5ce634a6bff61e48fdbcec19b2c08bf91f3aaae29c78d8028ac79d8679afac28`;
- evidence manifest `b1072eea1601449812ed8c9cccd02cc3a0214e0f8b26fac2b10616af89db72de`.

W evidence `logicalPath` jest wirtualną nazwą zdekompresowanego JSON, nie drugim plikiem w repo.
Ścisły evidence manifest schema v1 wiąże fizyczne archiwa `.json.gz`, ich logiczną treść, combined
oraz producer snapshot. Celowo nie ma w nim dodatkowych kluczy snapshot/inventory, bo source-bound
repeat validator odrzuca zmianę tego schematu. Te dwa są wiązane osobno przez stałe ścieżki i SHA w
`verify_local_cloud_quality_artifacts.py`; ten oracle jest uruchamiany przez test Rust razem z
mutation selftestami obu plików. Snapshot pozwala odtworzyć każdy `casePayloadSha256`, a inventory
publikuje wszystkie 728 unikalnych stringów z 4 744 wystąpień wraz z commitmentami ścieżek per
repeat. Raw reports mają schema v9, combined schema v5, inventory schema v2.

Finalne runy wiążą base commit `57ced723867d2a6612d4a61b67cdad4413bafdd6`, source fingerprint
`c8169e4ac77327ca3f7ad232e2e843f163efbc62a04afe664407f1a929dd7ec3`, manifest
`21ea3cc236b8c4058f18043b538bac87e93e933c86bd8b0c3696f5b67d45f01d`, evaluator
`b0feaaff5cb533fa2767a60ecd70844d3af3ee2f79be2b09b401eebf7b5f9363`, fixture
`b5f63efbc135a8629366614444bdba8d9501e28209d054e967b8e9debeddd9b2` i validator
`48d66559c713cc0da346d5d92fc0051be4b67ebb1b6b6f4422e3024101312198`.

Runtime local to `murmur-brain-workspace-build`, SHA-256
`a47b8ec18d8597f59caad79aedf9aa9c7d86a5e2444ff036785ddea4fd2d4c37`; runtime cloud to
`codex-cli 0.146.0`, SHA-256
`ae1d3ffe6d48aec6a4dc3f50e7eb8e0d11962485a6a9406c5a7012139383da02`. Wagi 4B miały
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

Real-model R1 zakończył test sukcesem; zewnętrzny runner zalogował `finished in 699.23s`. R2
zapisał kompletny raport, a test zakończył się `ok` (`1 passed`, `0 failed`); runner zalogował
`finished in 756.83s`. Dopiero potem wrapper zgłosił `survivor group 10069 still owns the lane via
guardian 46993` i `supervisor error: [Errno 1] Operation not permitted`. Te czasy i komunikat
guardiana są obserwacją z zewnętrznego logu runnera, nie elementem związanym przez evidence manifest.
Python validator przyjął oba pełne artefakty i odtworzył final combined. Deterministyczny replay,
pełne bramki projektu i dokładny werdykt Harnessu są raportowane w PR/receipt; nie należy mylić
samego uruchomienia modeli z końcowym PASS zmiany.
