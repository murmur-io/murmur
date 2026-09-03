# Murmur 2.3.1 — pełna analiza aplikacji

**Data:** 2026-09-02 · **Baza:** trunk `392fd223` (v2.3.1) · **Serwer:** `../murmur-server` @ `3ef670d` (= pin `.murmur-server-revision` = `origin/main`)

Zakres, o który prosił właściciel: architektura brain i połączenia między encjami, MCP i łączenie kontekstu, wycieki pamięci, nagrywanie/transkrypcja, stabilność, gotowość produkcyjna, „czy to naprawdę brain", org sharing — plus własna pogłębiona analiza.

> **STATUS (2026-09-03) — ten dokument opisuje stan 2.3.1, który już NIE OBOWIĄZUJE.**
>
> Audyt był podstawą programu naprawczego; **31 zmian wjechało na trunk i zostało wydanych jako
> [v2.4.0](https://github.com/murmur-io/murmur/releases/tag/v2.4.0)** (2026-09-03). Dlatego publikacja
> jest bezpieczna dopiero teraz: opis mechaniki dziury przed dostępną łatką byłby mapą dla atakującego
> na każdą zainstalowaną kopię.
>
> Trzy ustalenia o najwyższym ciężarze, wszystkie **naprawione i wydane**:
> **O1** rotacja klucza org po usunięciu członka nigdy się nie kończyła (PR #652) ·
> **O2** brak pinowania klucza przy zaproszeniu do org, czyli podmiana `pk_enc` przez relay (PR #667) ·
> **O3** zmodyfikowany klient mógł zablokować cudzy revoke/erase (naprawione po stronie serwera,
> `murmur-server` PR #22, wdrożone).
>
> Oceny w tabeli poniżej są historyczne — nie odczytuj ich jako bieżącego stanu aplikacji.

---

## 0. Werdykt w jednym ekranie

| Obszar | Ocena | Jednym zdaniem |
|---|---|---|
| Brain — substrat (retrieval, fakty, MCP) | **8/10** | Trzy-nogowy hybrydowy retrieval z bramkami widoczności, bitemporalne fakty, MCP z rewalidacją TOCTOU — to jest zrobione porządnie. |
| Brain — to, co widzi użytkownik po świeżej instalacji | **5/10** | Bez `claude` CLI + zgody na cloud + pobranego e5 „brain" to wyszukiwarka FTS ze złożonymi diakrytykami. |
| Połączenia między encjami | **5.5/10** | Graf linków jest realny i wpływa na odpowiedzi, ale lock / trash / delete **kasują** najcenniejsze krawędzie (linki ręczne, fakty, historię Ask) bez odtworzenia. |
| Wycieki pamięci | **8/10** | Zmierzone: FE płaskie przy 40× „Ask Brain" w edytorze; Rust ograniczony z konstrukcji; jedna niewiadoma to sidecar (mistralrs autorelease). |
| Nagrywanie i transkrypcja | **7/10** | Odporność wzorowa (crash-safe spool, watchdog, terminal guard); **diaryzacja, klasyfikacja echa i AEC są martwe na ścieżce produkcyjnej** od 2026-07-23, a przełączniki w Settings są bez efektu. |
| Stabilność / prod-ready (solo, local-first) | **7.5/10 — READY z zastrzeżeniami** | Zero paniki na ścieżkach użytkownika, migracje addytywne, guardy na każdym kasowaniu; zastrzeżenia: utrata klucza Keychain = utrata bazy, komendy sync na głównym wątku, brak ochrony gałęzi na GitHubie. |
| Stabilność / prod-ready (zespół, Shared Brain) | **5/10 — NOT READY** | Rotacja klucza po usunięciu członka **nigdy się nie kończy** (415 z relay), brak pinowania klucza przy zaproszeniu, zmodyfikowany klient może zablokować cudzy revoke/erase. |
| Frontend (Angular zoneless) | **7.5/10** | Reguły wiążące spełnione co do litery, zero dryfu IPC; dwie listy główne renderują bez okna, ~1.7k linii martwych komponentów, trzy słowniki hierarchii. |
| **Całość** | **6.5/10** | Inżynieryjnie ponad przeciętną tej klasy produktów; produktowo obiecuje więcej, niż domyślna instalacja dostarcza, a cykl życia danych (lock/trash/delete) niszczy wiedzę, którą brain buduje. |

**Czy to jest prod ready?** Dla jednego użytkownika local-first, który zainstaluje Claude CLI i pobierze modele: **tak, z zastrzeżeniami** (lista w §5). Dla zespołu na Shared Brain: **nie** — trzy blokery w §6.

---

## 1. Metoda i granice

- Siedmiu równoległych, niezależnych śledczych (encje/linki, brain+MCP, wycieki, nagrywanie, stabilność, org sharing, FE), każdy tylko do odczytu, z obowiązkiem cytowania `plik:linia`. Każda teza nagłówkowa poniżej została przeze mnie ponownie sprawdzona w drzewie (grep/sed), nie przepisana z raportu.
- **Uruchomione:** pełna suita Rust lokalnie (`3575 passed, 0 failed, 18 ignored`, 202 s); pomiar wycieków FE w Chromium na zamockowanym IPC (Playwright, 4 scenariusze); sonda MCP działającego release'u (tylko nieuwierzytelniona powierzchnia); odczyt stanu CI i ruleset na GitHubie.
- **Nie uruchomione:** dev-app z prawdziwym backendem obok zainstalowanego Murmura — **wspólny single-instance lock** (`instance_lock.rs:45-50`, ścieżka `…/com.meetnotes.app/murmur.instance.lock` nie jest dev-scoped) odmawia startu z systemowym alertem „Murmur is already running". W efekcie RSS sidecara przy wielokrotnym Ask pozostaje **niezmierzony** (recepta w §3).
- Serwer czytany z obiektu git `3ef670d`, nie z working tree.
- Dokumentacja w repo nie była źródłem żadnej tezy (rozjeżdża się z kodem w kilku miejscach — §2.3).

---

## 2. Brain: architektura, kontekst, MCP

### 2.1 Co jest naprawdę dobre

- **Retrieval** (`storage/db.rs::search_hybrid_visible`, ~`:5133-5160`): FTS5/BM25 + KNN (sqlite-vec, e5-small 384d) + noga grafu encji (ko-wzmianki, bez LLM), fuzja 0.4/0.4/0.2 z redystrybucją pustej nogi. Każda noga przez `visibility_clause`. Wektory ze stubu **nigdy** nie trafiają do bazy (`embed.rs:604-609`, `pipeline.rs:3403-3405` `should_auto_index`).
- **Fakty bitemporalne** (`facts.rs`, invalidate-not-delete), konsolidacja co godzinę czytająca z **pustym** zbiorem unlocków (`memory.rs:518`) — wiedza pochodna nie wynosi treści zapieczętowanych.
- **MCP** (`mcp.rs`): własny HTTP/1.1 + JSON-RPC 2.0, bind tylko `127.0.0.1:8765`, allowlista `Host`/`Origin`, token wymagany dla **każdej** metody łącznie z `initialize` (`mcp.rs:1618-1628`, porównanie constant-time), fail-closed przy zatrutym configu. Migawka `seal_epoch` + unlock set + `ask_dispatch_generation` brana pod mutexem cyklu życia i **rewalidowana przed zapisaniem pierwszego bajtu** (`mcp.rs:172-188, :919-946`). Sonda na żywo: `initialize` i `tools/list` bez tokena → `-32001`; zły `Host` → 403. 20 narzędzi, każde przez czytnik `*_visible` (`tools.rs:720-1560`).
- **Ask agentowy** (`commands/ask.rs:279`, `agent.rs`): 6 kroków, budżety 4k/64k/32k, `GatedToolExecutor` bez zapisów, fallback do deterministycznego floora; cache odpowiedzi na dashboardach wstrzymuje się, gdy zbiór czytelnych folderów się zmieni (`dashboards_store.rs:704-740`).
- Ustalenia audytu z 2026-08-13: budżet kontekstu dla lokalnego brain (**naprawione**, `vault_context.rs:108-110`), limit znaków historii (**naprawione**, `vault_chat.rs:10` = 16 000).

### 2.2 Co reaczy dostaje użytkownik po świeżej instalacji

Domyślne wartości (`settings/config.rs:674-746`): `provider_id = "claude_code"`, `brain_backend = Cloud`, `cloud_egress_consented = false`, `semantic_search_enabled = true`, `vault_path = None`, model whisper wybierany maszynowo (`small` lub `large-v3-turbo-q8_0`).

| Warstwa | Wymaga | Bez tego |
|---|---|---|
| Notatka AI, timeline, encje, fakty, hinty | zainstalowany `claude` CLI na PATH powłoki logowania (`claude_code.rs:643-707` → `Unavailable`) **i** zgoda na cloud | transkrypt + FTS, nic więcej. Graf encji jest ekstrahowany przez **LLM roli Notes** (`commands/mod.rs:5426-5442` → `summarize/graph.rs`), nie przez lokalny NER (DeBERTa służy tylko do redakcji). |
| KNN, linki semantyczne, „Related by meaning" | pobrany model e5 (nie jest w bundlu) | noga KNN pusta; `StubEmbedder` |
| Fakty użytkownika, rollupy, reranker | lokalny reasoner (GGUF w sidecarze) | puste fakty, zero rollupów, reranker = identyczność |
| Automatyczne `[[wikilinki]]` w notatce AI | skonfigurowany vault Obsidian (`pipeline.rs:2289-2290` → `list_vault_titles`) | prompt dostaje `(none)`, zero automatycznych krawędzi meeting↔note |
| Tasks | konto + organizacja (`commands/tasks.rs:11-14`) | użytkownik local-first **nie ma Tasks** |

Wniosek: substrat jest na 8, ale **ścieżka domyślna kompletuje prawie nic automatycznie**. To największa różnica między tym, co obiecuje landing, a tym, co dostaje osoba po `open Murmur.dmg`.

### 2.3 Znaleziska

| # | Waga | Znalezisko | Dowód |
|---|---|---|---|
| B1 | **High** | Opublikowany snippet MCP **nie działa**: README i landing podają wpis bez tokena `{ "url": "http://127.0.0.1:8765" }` pod nazwą `claude_desktop_config.json`, a serwer wymaga bearera dla każdej metody; Claude Desktop nie akceptuje gołego `url` (potrzebny mostek stdio / Custom Connector). Poprawny JSON generuje tylko „Copy config" w Settings (`commands/mod.rs:6825-6850`). | `README.md:528-533`, `landing/docs.html:871-877`, sonda `-32001` |
| B2 | **Medium** | Floor Ask karmi KNN wektorem ze **stubu** przy `min_cosine = 0.0` (`ask.rs:568` `active_admitted_embedder` + `vault_context.rs:197`), podczas gdy MCP używa uchwytu real-only i progu 0.78. Nieszkodliwe przy pustym `vec_chunks`; po usunięciu modelu z rezydentnym indeksem noga 0.4 wagi dostaje szum haszowy. | j.w. |
| B3 | **Medium** | Przekroczenie 10 s deadline'u w MCP kończy się **pustym 202**, nie błędem JSON-RPC (`mcp.rs:914-921`); jedno `Mutex<Connection>` (`db.rs:380`) dzielone z UI i tickami tła. | j.w. |
| B4 | **Medium** | Brak testu end-to-end z prawdziwym klientem MCP (57 testów jednostkowych + 3 e2e statusu). | — |
| B5 | **Low** | FTS: `unicode61 remove_diacritics 2`, brak stemmera → „spotkanie"/„spotkania" to różne tokeny; w realnym vaultcie 18/20 trudnych zapytań miało pustą nogę FTS (pomiar 2026-07-10, od tamtej pory nic nie mierzono). | `db.rs:2060-2076` |
| B6 | **Low** | Popover „Ask Brain" w notatce: FE odrzuca spóźnione odpowiedzi (`requestSeq`), backend odmawia równoległej tury per notatka, ale **nie anuluje** poprzedniego wywołania cloud — zastąpione zapytanie liczy się do rachunku. | `note-brain-popover.component.ts:349-381`, `enrich.rs:565-575` |
| B7 | **Docs** | `ARCHITECTURE-LOCAL-CLOUD.md:13,46` („no auth"), `USE-WITH-YOUR-AGENT.md:37` („nine tools"), `PHASE0-PLAN.md:81` — nieaktualne. | — |

Otwarte z audytu 08-13: pakowanie całych notatek zamiast fragmentów dowodowych, brak typowanej proweniencji, `knowledge_diff`/`list_entities` dostępne tylko przez MCP (bez wywołań w aplikacji), reranker wciąż bez zmierzonej wartości.

---

## 3. Wycieki pamięci

### 3.1 Pomiar (Playwright, Chromium, IPC zamockowane, GC wymuszany przed próbką)

| Scenariusz | it. 1 | it. 10 | it. 20 | it. 30 | it. 40 | Ocena |
|---|---|---|---|---|---|---|
| A. Edytor notatki: zaznacz → Ask Brain → Refine → Accept (×40) | 1373 węzłów / 101 listenerów / 19.6 MB | 1373 / 101 / 19.6 | 1373 / 101 / 19.6 | 1373 / 101 / 19.6 | 1373 / 101 / 19.6 | **idealnie płasko** |
| B. Strona Ask, 40 pytań w jednym wątku | 873 / 50 / 16.3 | 1134 / 68 / 16.3 | 1424 / 88 / 16.3 | 1714 / 108 / 16.3 | 2004 / 128 / 16.3 | +29 węzłów i +2 listenery na wiadomość = wiersze wątku (z konstrukcji); sterta płaska |
| C1. Ta sama zakładka meeting ↔ lista (×30) | 1396 / 104 | 1388 / 104 | 1388 / 104 | 1388 / 104 | — | płasko |
| C2. 4 różne meetingi przez router (×30) | 1771 / 174 | 2817 / 384 | 2801 / 384 | 2801 / 384 | — | rośnie do 4 zatrzymanych zakładek, potem płasko (retencja zakładek z konstrukcji) |

Zero błędów konsoli w żadnym scenariuszu. **Odpowiedź na pytanie „czy odpalanie brain w tekście kilka razy cieknie":** po stronie webview — **nie**.

### 3.2 Rust (analiza statyczna, zweryfikowana)

Ograniczone z konstrukcji: jeden slot embeddera (`embed.rs:408-450`, forward w `autoreleasepool`, zwalniany na czas nagrania), jeden slot reasonera (`reason.rs:680-964`), sidecar = jedno dziecko, stderr `Stdio::null()`, idle-kill 300 s (`reason/sidecar.rs:287-306, 888-899`), whisper `Transcriber` per przebieg, ring nagrywania 32 MB, bufory live przycinane z przodu (16 000 / 4 000 znaków), historia Ask 12 tur / 64k znaków przy odczycie, każda pętla tła spawnowana **raz** w `.setup`, `[profile.release]` obecny (`Cargo.toml:47-50`). Główny proces: **RSS nie rośnie per wywołanie** (High).

Kandydaci:

| # | Prawd. | Co | Dowód / jak potwierdzić |
|---|---|---|---|
| L1 | Medium | **Sidecar mistralrs**: cały wątek silnika owinięty jednym `autoreleasepool`, drenowanym dopiero przy wyjściu wątku (`mistralrs-core-0.8.1/src/lib.rs:570-573`); jedynym „drenem" jest idle-kill po 300 s. Seria Ask w odstępach < 5 min nigdy go nie odpala. | `while :; do ps -o rss= -p $(pgrep -x meetnotes-brain); sleep 5; done` przez 1 → 10 → 30 zapytań; monotoniczny wzrost resetujący się po 5 min bezczynności = to. |
| L2 | High (z konstrukcji) | Zatrzymane drzewa zakładek bez limitu liczby (`tab-route-reuse.strategy.ts:29`); każda zakładka detail trzyma DTO + pełny transkrypt po otwarciu zakładki Audio (`detail.component.ts:580`) + `<audio>`. | Activity Monitor „Murmur Web Content": 20 meetingów z Audio → RSS → zamknij wszystkie → RSS. |
| L3 | Low | `verify_cache` w `AppState` rośnie per zweryfikowany meeting do relocka (`state.rs:319`, `enrich.rs:119-121`). KB. | — |
| L4 | Low (churn) | Nowy `reqwest::Client` per wywołanie providera cloud (`summarize/anthropic.rs:94-101`, `ollama.rs:66`) — TLS handshake per Ask, brak retencji. | `nettop -p Murmur` |
| L5 | Medium | `DestroyRef.onDestroy` rejestrowany **po** `await` (NG0911, listener zostaje): `onboarding.component.ts:294-301`, `brain-enable-card.component.ts:96-103`, `settings.store.ts:1840-1881`. Wzorzec poprawny jest w `record.component.ts:470-499`. | — |
| L6 | CPU | Pętla invalidacji przypomnień odpytuje SQLCipher co 250 ms w nieskończoność (`commands/reminders.rs:148, 2752-2788`) — 4 round-tripy/s na idle. | — |

Dane z release'u: zainstalowany Murmur 2.3.1 po 26 h bezczynności siedzi na **127 MB RSS** — idle nie jest problemem.

---

## 4. Nagrywanie i transkrypcja

### 4.1 Co jest solidne

Callback RT nie alokuje i nie blokuje (atomic ring 32 MiB, `audio/recorder.rs:56-96`), mic spoolowany do `recording-inflight/<gen>.mic.f32` z checkpointem `(inode, frames, sha256)` w SQLCipher, ring nigdy nie jest przycinany przed trwałym dowodem (`recorder.rs:406-440`). Stop jest single-flight z 20 s timeoutem oddającym własność kontynuacji w tle, `TerminalStatusGuard` gwarantuje terminalny status nawet przy unwind (`pipeline.rs:150-283`). Watchdog 100 ms po stronie backendu auto-stopuje przy błędzie capture. Drabina odzyskiwania przy starcie w bezpiecznej kolejności (`lib.rs:591, 691-748`). Watchdog ASR `max(15 min, 4×audio)`. Segmenty zapisywane atomowo, status `Transcribed` **przed** summarize. Dekoder batch: beam-5, drabina temperatur, bramki entropii/logprob/no-speech, flash-attn, VAD Silero. Pętla live nie dotyka archiwum (governor termiczny tylko `live.rs`). Z RCA 2026-07-21 wylądowało: sidecar zabijany + cache embeddera/NER/reasonera zwalniane na Start i Stop, domyślny `turbo-q8_0`, `[profile.release]`. **Nie wylądował** guard RAM na `Transcriber::load`.

### 4.2 Znaleziska

| # | Waga | Znalezisko | Dowód |
|---|---|---|---|
| R1 | **High (jakość)** | **Diaryzacja, klasyfikacja echa, pomiar offsetu i offline AEC są martwe na ścieżce produkcyjnej.** Stop dispatchuje `pipeline::run_file_backed` (`commands/mod.rs:2209`), a `run_file_backed_inner` zwraca `Ok((merge_streams(streams), 0usize))` z komentarzem „Full-recording sherpa diarization is deliberately skipped" (`pipeline.rs:1070-1078`). Całe `Diarizer::load` / `relabel_others` / `classify_cross_stream_echo` / `estimate_stream_offset` / `cancel_echo_offline` żyje w `run_inner`, osiągalnym tylko przez `run_after_stop`, którego jedynym wywołaniem jest legacy salvage ≤120 s (`audio/spill.rs:2151`). Skutki: na głośnikach każde zdanie rozmówcy pojawia się dwa razy (`others` + echo jako `me`), strona zdalna nigdy nie jest dzielona na `others-N`, przełączniki `diarize_others` (domyślnie ON), `voiceprint_enabled`, `aec_enabled`, `post_aec_enabled` **nic nie robią**. | zweryfikowane grepem |
| R2 | **High (UX)** | `retry_transcription` istnieje w backendzie (`commands/mod.rs:7555-7680`), watchdog i terminal guard każą użytkownikowi „use Retry transcription", a **w FE nie ma żadnego wywołania** — tylko definicja w `ipc.service.ts:1259`. Meeting w stanie `Error` z zarchiwizowanym audio jest nie do odzyskania z UI. | zweryfikowane grepem |
| R3 | Medium | Twarde okna dekodowania 120 s bez zakładki (`pipeline.rs:1160-1166, 1394-1412`): ~30 cięć w środku słowa na godzinę, reset kontekstu na granicy. | — |
| R4 | Medium (PL/EN) | `language: None` → whisper wykrywa język **per region VAD** na pierwszych 30 s; krótki polski region rozpoznany jako angielski wychodzi jako bełkot. Onboarding zapisuje `""` = auto. | `config.rs:676`, `onboarding.component.ts:116` |
| R5 | Medium | Watchdog ASR przywraca UI, ale osierocone dekodowanie trzyma heavy-semaphore i lease modelu; następny Start kończy się „local AI did not quiesce in time" do restartu aplikacji. | `pipeline.rs:306-315`, `perf.rs:920-940`, `commands/mod.rs:1266-1285` |
| R6 | Medium | Brak sygnału w trakcie nagrania, że system-audio jest martwe/odmówione (TCC exit 3 odkrywane dopiero na Stop; `RecordingStatus` niesie tylko `recording/meeting_id/started_at`). Godzinny call nagrany „obustronnie" ma tylko `me`. | `commands/audio.rs:31-43`, `source.rs:1671-1680` |
| R7 | Low→Medium (niezweryfikowane) | Odpięcie urządzenia wejściowego: jedyny nasłuch to callback błędu cpal; brak listenera zmiany domyślnego wejścia (obserwowane jest tylko wyjście, `audio/output.rs:65-71`). | wymaga podpisanego Maca |
| R8 | Medium (test) | Produkcyjna ścieżka ASR nie ma oracle'a end-to-end: `scripts/e2e-mix.sh` → `examples/e2e_core.rs` używa in-memory `audio::mix` + profil Fast + `Some("en")`, nigdy `transcribe_raw_windows`/`merge_streams`/`publish_mix`/profil Accurate. | — |
| R9 | Low | Brak filtra halucynacji post-hoc; miks archiwum ścisza o 6 dB miejsca, gdzie oba strumienie nakładają się; cichy meeting = `Error` + toast zamiast stanu „pusty". | — |

Utrata treści na happy path: **nie znaleziono**.

---

## 5. Stabilność i gotowość produkcyjna

### 5.1 Mocne strony

- **Dyscyplina paniki jest realna.** Po wykluczeniu `#[cfg(test)]`: ~25 `unwrap/expect/unreachable` w kodzie shipowanym, każdy dowodliwie niezawodny (statyczne regexy, `expect("checked above")` po guardzie). 4 135 surowych `unwrap()` to moduły testowe. `Db::lock` odzyskuje poison (`db.rs:2890-2894`), 97 miejsc `PoisonError::into_inner`, ciało pipeline w `catch_unwind`.
- **Start nie może się wywrócić:** flock instance-lock zwalniany przy crashu, awaria DB/Keychain = natywny dialog + czyste wyjście, salvage na `spawn_blocking`.
- **Migracje od v2.0.0 są w 100 % addytywne** (`db.rs:2586-2735`; jedyny backfill z guardem `WHERE parent_container_id IS NULL`). `encrypt_in_place` nadal backup → eksport → weryfikacja → rename.
- **Każde kasowanie w tle ma guard:** purge Trash fail-closed na niedatowalnym `deleted_at` i omija zapieczętowane; auto-prune audio domyślnie OFF; sweepy wiekowane 1 h; prune logów dotyka tylko logów.
- **Bezpieczeństwo:** CSP `script-src 'self'`, `style-src` chroniony (T4), asset scope tylko `audio/*.wav` z odmową `.enc`; brak pluginu fs/shell; `env_clear` na dzieciach claude_code/sidecar/afm/calendar/system/aec; codex pod `sandbox-exec`; zero materiału sekretnego w `tracing`.
- **Testy i CI:** 3 593 testy Rust (18 ignorowanych), 116 plików e2e / 499 testów × 2 silniki w 6 shardach, `clippy -D warnings`, `cargo audit`, `cargo deny`, selftesty control-plane.

### 5.2 Znaleziska

| # | Waga | Znalezisko | Dowód |
|---|---|---|---|
| S1 | **High** | **238 z 352 komend Tauri to synchroniczne `fn`** (m.in. `list_meetings`, `get_meeting_detail`, `search_meetings`, `get_graph`, `list_notes`) — Tauri wykonuje je na głównym wątku; wszystkie biorą jedno `Mutex<Connection>` dzielone z konsolidacją, purge Trash, reindexem, org sync i wątkiem MCP. Długa transakcja (kaskada `delete_meeting` `db.rs:3849`, ingest org) zamraża każde kliknięcie na czas jej trwania. To ta sama klasa co „org panel hanging" z #647. | policzone grepem |
| S2 | **High** | **Utrata DEK z Keychain = utrata bazy, bez recovery.** `get_or_create_db_dek` (`keychain.rs:81-150`) odmawia mintowania tylko przy `-34018` (mis-signed); przy innym „absent" mintuje nowy DEK, istniejąca baza nie otwiera się → dialog „contact support". Brak eksportu klucza / recovery-key w Settings. KEK ma analogiczny guard („nie mintuj, gdy istnieją zapieczętowane foldery"), DEK — nie. Mitygacja: wyeksportowane `.md` w vaultcie. | j.w. |
| S3 | **High (governance)** | **Gałąź `murmur` nie ma dziś żadnej ochrony po stronie GitHuba:** ruleset „Protect" ma `enforcement: disabled`, classic protection zwraca 404. Własny audyt CI (`remote harness boundary`) pada na każdym PR dependabota z tego powodu (run 33665646637). „CI jest jedynym autorytetem merge" z CLAUDE.md nie jest egzekwowane zdalnie; egzekwuje to tylko lokalny hook. | `gh api repos/murmur-io/murmur/rulesets` |
| S4 | Medium | **Nieskonsentowany egress przy starcie:** `checkOnStartup()` (`app.component.ts:136` → `update.rs:123-162`, `api.github.com`) bez opt-out i bez wpisu w ledgerze. Bez treści, ale sprzeczne z obietnicą „cloud tylko za zgodą". | j.w. |
| S5 | Medium | **Cicha porażka jako sukces** w warstwie zespołowej: `shared-workspace.service.ts:152-154` mapuje błąd `listSharedWorkspace` na `null/[]` (awaria org = pusty workspace), `shareRewrapPending().catch(() => undefined)`, porzucone błędy czyszczenia załączników (`note-editor.component.ts:1367,1383`). 25 połkniętych `catch`, 5 `void this.ipc.*` bez handlera. | j.w. |
| S6 | Medium | Diagnostyka: log żyje 24 h (`applog.rs:49, 187-215`), brak crash reportera; „wczoraj się wywaliło" jest już wyczyszczone. | j.w. |
| S7 | Medium | **Flaky test na trunku:** run CI dla merge'a #647 padł na `collaborator_advanced_head_is_terminal_even_when_head_scan_is_unavailable` (`lifecycle_tests.rs:30364`), kolejny run zielony. Lokalnie cały moduł przechodzi. | `gh run view 33550375752` |
| S8 | Medium | Kształt testów: `lifecycle_tests.rs` 42 321 linii / 650 testów w jednym module `#[path]`; ten wzorzec już raz „ukradł" `#[cfg(test)]` (commit `6641e516`), czego `cargo test --lib` nie widzi. | — |
| S9 | Medium | Tempo: 130 commitów bez merge'y między v2.0.0 (08-27) a v2.3.1 (09-01), z czego **57 `fix:`** (44 %); pięć tagów w sześć dni, same-day hotfixy Trash/org/Settings. Funkcje lądują szybciej, niż zdążą się uleżeć. | `git log` |
| S10 | Low | Bez pluginu updatera — ręczne sprawdzenie GitHub Releases + przeglądarka; `hono` w lockfile to zależność `@angular/cli → @modelcontextprotocol/sdk`, nieużywana w aplikacji; komenda testowa `reminder_runtime_probe_control` w produkcyjnej liście handlerów (`lib.rs:423`) bez `cfg(debug_assertions)`; dzieci PATH-relative (`ps`, `sysctl`, `open`, `osascript`) obok rodzeństwa z `/bin/…`; klient MCP konektorów dziedziczy pełne env (`connectors/mcp_client.rs:293-300`). | — |
| S11 | Low | Dev i release **dzielą** single-instance lock (`instance_lock.rs:49`, brak `app_dir_name()`), więc „dane dev i release są izolowane" nie obejmuje procesu — dev-app nie wystartuje obok zainstalowanego. | zweryfikowane empirycznie |

### 5.3 Onboarding

Kreator: welcome → model → provider → brain → vault → done; konto nigdy nie jest wymagane, vault opcjonalny. Model whisper `small` ~470 MB / `large-v3-turbo-q8_0` ~875 MB z HF z wznawianiem `.part`; offline = brak transkrypcji do czasu pobrania. Brak kroku uprawnień — mic / screen-recording pojawiają się leniwie przy pierwszym nagraniu. Onboarding ma 4 `catch` tylko z komentarzem, 0 toastów, 0 szablonów renderujących błąd — awarie tam są niewidoczne.

---

## 6. Org sharing (Shared Brain)

### 6.1 Co jest solidne

Autoryzacja serwera jednolita i w transakcji (`AuthedUser`; nie-członek → 404; PUT/PATCH/DELETE dokumentów re-sprawdzają członkostwo i uprawnienie pod `FOR UPDATE`). **Wylądowało od sierpnia:** `docId` end-to-end + CAS (`PUT /documents/{docId}` z `expectedRev` → 409, viewer trzyma draft), uprawnienia View-only / Can-edit per dokument, prywatne linki lokalne do dokumentów org (`LinkKind::Org`), kolejność bramek publikacji (lock gate → zgoda → journal → seal + open-verify → pre-check rozmiaru → ledger → POST → rekonsyliacja tylko przez GET), ingest fail-closed w dobrej kolejności, jedna prymitywa ewikcji (plaintext, chunki, wektory, FTS, załączniki, projekcja tasków, historia Ask), Tasks w pełni szyfrowane OCK, blokery #6/#7 z audytu 2.0 naprawione.

### 6.2 Status pozycji z poprzednich audytów

| Pozycja | Status |
|---|---|
| Rotacja klucza po usunięciu członka | **OTWARTE — nigdy się nie kończy.** `share/client.rs:1100-1116` POST-uje `/generation` **bez body**, route ekstrahuje `Json<BumpGenerationRequest>` → 415 → toast `sharing-rejected` **po** usunięciu członka. Grant nadal tylko dla ownera, serwer wymaga każdego aktywnego członka. Żaden test nie woła `org_remove_member_inner`. |
| Purge po usunięciu z jedynej org | Częściowo: korroboracja per org przez 404, ale bramkowana `org_egress_consented` — invitee bez zgody trzyma replikę + OCK na zawsze. |
| Re-check integralności bloba na GET | Otwarte (klient AEAD pokrywa). |
| Stabilna tożsamość `docId` | **Naprawione.** |
| Uprawnienia / CAS / linki (08-12) | **Wysłane.** |
| Unshare / org delete / audit UI | Częściowo (folder, kontener, delete autora); `revokeOrgShare` bez wywołania w UI; brak metody klienta dla `DELETE /v1/orgs/{id}` i audytu. |
| Zasięg brain | Osobna partycja (z konstrukcji): jeden szew `search_org_brain_hits` bramkowany członkostwem ∧ `context_enabled`; widoczne dla Ask, MCP `org_search`, dossier, Related, tasków; nie dla eksportu/analytics/faktów. |
| #647 „org panel hanging" | Fix **objawowy**: ograniczono tylko `org_refresh` (10 s), a przyczyna to globalny `org_share_mutation_lock` (`state.rs:352`) z ~45 nieograniczonymi holderami — tick 60 s trzyma go przez 4×30 s HTTP **i** nietimeowany permit `heavy_inference` za transkrypcją; `unlock_folder` trzyma go przez odczyt Touch ID. Naprawy diagnostyki (tagowane `brief_err`, Auth nie połykane) są prawdziwe. |

### 6.3 Nowe znaleziska

| # | Waga | Znalezisko |
|---|---|---|
| O1 | **Critical** | Rotacja martwa i w złej kolejności (wyżej). W zespole 3–10 osób każde usunięcie kończy się błędem; org zostaje na generacji N; usunięty członek z zapamiętanym OCK odszyfruje wszystko, co opublikowano później, jeśli zdobędzie ciphertext (jedyną barierą jest bramka członkostwa relay). |
| O2 | **High** | Zaproszenie do org **nie pinuje klucza**: `org.rs:3567-3596` owija OCK dowolnym `pk_enc` z `lookup_key`, bez `tofu_check` (który mode-B ma w `mod.rs:14282`). Złośliwy relay podstawia klucz i czyta całą org. „Zero-knowledge" trzyma się tylko wobec relay honest-but-curious. |
| O3 | **High** | Legacy publish przez `blobId` aliasuje cudze bloby (serwer sprawdza tylko `blob_exists`, FK `NO ACTION`); `revoke_owned`, `soft_delete_org_and_gc`, `erase_account` robią bezwarunkowe `DELETE FROM blobs` → naruszenie FK → 500. Zmodyfikowany klient współczłonka może **zablokować cudze revoke / org delete / erase konta**. |
| O4 | **High** | Globalny mutex org nadal nieograniczony (wyżej): długa transkrypcja lub wiszący prompt Touch ID stopuje `start_recording`, `move_note`, `delete_note`, `lock_folder`, `org_sync_now`. |
| O5 | Medium | Legacy POST bez `docId` omija in-tx check członkostwa/generacji; `contentSha256` = SHA-256 **plaintextu** koperty — wyrocznia „potwierdź zgadywanie" i klucz łączenia między użytkownikami; 401 poza refreshem zostawia sesję „zalogowaną"; receipts mutacji niewykorzystane (`mutation_id: None` „bo relay ich nie ma" — ma, migracja 0011); brak retry/backoff (jedna akcja sieciowa na tick 60 s); e-maile członków + nieograniczony audit serwowane każdemu członkowi. |
| O6 | Medium | Tombstone kontenera na ścieżce live-pull nie dotyka `org_containers` — otrzymany Space wisi do 6 h sweepu; auto-publish nowych elementów w udostępnionym kontenerze jest next-tick (≤60 s), nie natychmiastowy. |

**Co relay widzi w plaintext:** e-mail, platforma, id urządzeń, surowy sekret TOTP; **nazwa org**, id/role członków, e-maile członków (serwowane współczłonkom); `seq`, autor, `doc_id`, `rev`, generacja, `access`, `content_sha256` plaintextu, rozmiar ciphertextu, czasy, pełna linia receipts i audytu. Zaszyfrowane: tytuł, markdown, załączniki, placement, nazwa/emoji kontenera, pola tasków. Zero-knowledge = „ślepy na treść", nie „ślepy na metadane" i nie „odporny na fałszerstwo".

Testy: 650 lifecycle, 57 mock-relay, 17 kontenerów, 11 diagnozowalności; serwer ma realne testy dwu-kontowe. **Brak testu dwu-klientowego**; rotacja bez testu.

---

## 7. Frontend

Reguły wiążące spełnione co do litery: 0 `*ngIf/*ngFor`, 0 dekoratorów `@Input/@Output/@ViewChild`, 0 DI w konstruktorze, 0 `async` pipe, 0 `markForCheck`, 0 komponentów inline, 0 `console.error`; 26 wywołań `listen()` — każde z zachowanym `UnlistenFn` i `DestroyRef.onDestroy`; T2/T3/T4/T6 honorowane; **zero dryfu FE→Rust** (340 metod IPC vs 355 handlerów, jedyny „brak" to intencjonalny `rename`); `models.ts` bez pola snake_case.

| # | Waga | Znalezisko | Dowód |
|---|---|---|---|
| F1 | High | Dwie główne listy renderują **wszystko**: `list_meetings` zwraca każdy wiersz bez limitu (`commands/meetings.rs:188`), `library.component.html:421` i `notes-home.component.html:392` rysują pełną tablicę, każdy wiersz woła `formatDate()` tworzące świeży `Intl` formatter. `RENDER_CAP` istnieje tylko dla transkryptu (80), ludzi (100), map. Jank od ~500 pozycji. | — |
| F2 | High | 16 komend Rust bez wywołania w FE (cała powierzchnia zarządzania serwerami MCP `add/list/remove/test_mcp_server`, `consent_to_mcp_server`, `list_embed_models`, `select_embed_model`, `afm_available`, `entity_dossier`, `forget_entity_fact`, `list_open_commitments`, `org_sweep_pending`, `org_update_own_item`, `delete_companion_note_if_empty`, `reminder_runtime_probe_control`). | grep |
| F3 | High | ~1 700 linii martwych komponentów: `DashboardTileComponent` (421 TS + 668 SCSS, widok używa compose/read), `AiOrbComponent`, `MurCardComponent`, `MurInputComponent` (0 użyć `<mur-card>`/`<mur-input>`). | grep |
| F4 | Medium | Refetch-on-lock bez guardu kolejności w `graph.component.ts:169` i `people.component.ts:99` (relock → unlock może namalować starszy zbiór; przez chwilę widać zapieczętowane encje po relocku). `/people` i `/graph` łamią §8 (spinner chowa cache). | — |
| F5 | Medium | **Trzy słowniki hierarchii:** ten sam węzeł to *Workspace* (nav), *space* (toasty „Renamed space…"), *container* (4 komunikaty błędów), `level === "project"` (przecieka jako „People & projects"). Recordings vs Meetings vs `/library` vs „Capture". „Shared / Shared brains / Shared Brain(s) / Shared work" w czterech pisowniach. `check-vocabulary.mjs` pilnuje tylko żargonu deweloperskiego. | — |
| F6 | Medium | Tokeny kolorów omijane: `#9d7bff` ×7 bez tokena, `--graph-entity` istnieje a jest przepisany literałem, 117 surowych `rgba()`; motyw jasny i suwak `--glass-user-alpha` tam nie sięgają. 19 warstw `backdrop-filter: blur` (WebKit blur był już podejrzanym o lag w lipcu). | — |
| F7 | Low | Bajty NUL w źródle (`entity-detail.component.ts:240`) — plik traktowany przez grep jako binarny, więc **każdy audyt grepowy go pomija**; `standalone: true` w `stage2-panel.component.ts:34`; 10 duplikatów `formatDate()`; `track $index` na `ViewFilter` bez id. | — |

---

## 8. Czy Murmur to naprawdę „brain" i zbiór połączonych kontekstów?

**Tak — jako silnik. Nie — jako doświadczenie domyślne. I niebezpiecznie — jako cykl życia.**

1. **Graf jest realny i nosi wartość.** Jedna tabela `links` z rozwiązanymi id (odporna na rename), `UNIQUE(src,src_id,dst,dst_id,edge_type)`, tombstony, indeksy w obie strony (`storage/links.rs:188-203`), mutual-kNN z progami 0.80/0.88 i przycinaniem inbound, re-check seal w transakcji na obu końcach. Aktywne krawędzie **zasiewają zakres Ask** w czacie meetingu i notatki (`source-scope.service.ts:44-52` → `vault_context.rs:537-560`, do 8 sąsiadów), pakują kontekst przy konwersji do notatki, renderują Related i graf. Noga encji jest realnym składnikiem rankingu. Regresje z audytu 07-18 (tombstony wskrzeszane, krawędź companion kasowana) są **naprawione** (`LINK_DECISION_KEEP`, re-derivacja companion na unlock).

2. **Ale prawie każdy automatyczny producent stoi za CLI, pobraniem modelu albo vaultem** (§2.2). Domyślny „recorder" po instalacji kompletuje: transkrypt, FTS, krawędź companion i to, co sam wpisze w `[[…]]`. Dla porównania z CLI + zgodą: fakty, encje, commitments, hinty, sugestie przypomnień — czyli to, co sprzedaje landing.

3. **Cykl życia niszczy wiedzę, którą brain buduje** — to najważniejsze odkrycie tej analizy i **żaden z poprzednich audytów go nie widział:**

| # | Waga | Operacja | Co ginie bezpowrotnie | Dowód |
|---|---|---|---|---|
| E1 | **Critical** | **Lock → unlock folderu** | **Każdy link ręczny** dotykający folderu. `purge_links_tx` zostawia tylko `dismissed` lub `active AND created_by='accepted'` (`storage/links.rs:1640-1642`); link z `link_items` to `created_by='user'` bez markera w treści (`commands/links.rs:283-323`), a `rederive_links_for_folder` odtwarza tylko wikilink/semantic/companion/accepted. Komentarz w `mod.rs:5963` o „re-materializacji przez body re-index" jest nieprawdziwy. Test `purge_links_tx_drops_manual_on_seal` (`db_tests/tests.rs:13069`) **utrwala** tę utratę jako zamierzoną. | zweryfikowane |
| E2 | **High** | Lock folderu | Cały ledger faktów meetingów (`seal_store.rs:596-612`: `facts`, `user_facts`, `supersessions`, korekty, voiceprinty). Unlock re-derivuje **tylko linki**; knowledge diff, dossier, rollupy tracą historię, chyba że użytkownik ręcznie odpali ekstrakcję cloud. | zweryfikowane |
| E3 | **High** | Delete meetingu / dokumentu / folderu, seal, forget fact, zmiany org | **Cała** historia Ask (`purge_all_ask_conversations_tx` w `db.rs:3847, 4569, 5661, 6029, 7322`). Wyrzucenie jednego meetingu do kosza kasuje każdą rozmowę. Audyt 2.0 nazwał to „residual, out of scope" — nadal jest. | zweryfikowane |
| E4 | **High** | Trash → restore | Linki, wzmianki encji, fakty. Kosz to snapshot + hard-delete (wiersz, segmenty, notatki, timeline, tagi); delete purguje linki z `preserve=false`, kaskaduje `entity_mentions`, kasuje fakty; restore nie woła `index_wikilinks`/`auto_link`/`set_companion_link`. Przywrócony meeting wraca **odłączony**. | zweryfikowane |
| E5 | **High** | Brak vaulta | Zero automatycznych wikilinków w notatce AI (`pipeline.rs:2289-2290`). | zweryfikowane |

4. **Pojęciowe dublety bez wzajemnych linków:** Tasks (tylko org), commitments (parse `- [ ]` **tylko** z notatek meetingów, do 1000 notatek re-czytanych per wywołanie, `db.rs:8162-8200`), Reminders (kotwice meeting/note), fakty — cztery „to-do", żadne nie zna pozostałych. Dwa panele „Related" na jednym meetingu (stored links vs read-time KNN). Trzy drzewa kontenerów (`folders`, `org_local_placements`, `.murmur/tasks`). `memory_rollups` produkowane co godzinę, a czytane w aplikacji tylko przez purge/eksport — wartość dociera do użytkownika wyłącznie przez `.md` w vaultcie. `link_related_notes_inner` to no-op (`mod.rs:2689-2694`), a pipeline nadal spawnuje dla niego wątek (`pipeline.rs:2824-2846`). Linki **nie docierają** do Ask całego vaulta ani do MCP (brak narzędzia link/backlink).

**Ocena:** substrat 8, graf 5.5, doświadczenie domyślne 5. Naprawa E1–E4 (bez dotykania silnika) podnosi graf do ~7.5.

---

## 9. Własna pogłębiona analiza

### 9.1 Wzorzec „lock/trash/delete jako czarna dziura"

E1–E4 i O1 mają wspólny korzeń: **operacje cyklu życia były projektowane pod inwariant „nic nie wycieka", nie pod inwariant „nic nie ginie z wiedzy pochodnej".** Seal purguje wszystko, co pochodne (poprawnie dla bezpieczeństwa), a unlock odtwarza tylko podzbiór (linki), bo tylko dla linków ktoś napisał re-derivację. Trash odtwarza tylko snapshot wierszy. To nie jest bug w jednym miejscu — to brak **jednego inwariantu i jednego oracle'a**: *„dla każdej operacji odwracalnej (lock/unlock, trash/restore) zbiór krawędzi, faktów i historii po odwróceniu jest identyczny jak przed"*. Taki test (round-trip na `links`+`facts`+`ask_conversations`) nie istnieje; istnieje jego negacja (`purge_links_tx_drops_manual_on_seal`). Rekomendacja: zdefiniować `LifecycleRoundTrip` oracle w `db_tests/lock_tests.rs` obok `seal_transcript_timeline_round_trips_byte_identical`, a linki ręczne i fakty sealować-i-odtwarzać (jak notatki), nie purgować.

### 9.2 Domyślny provider zależny od cudzego CLI

`claude_code` jako domyślny provider oznacza, że **pierwszy „wow" produktu** (notatka AI po nagraniu) zależy od tego, czy użytkownik ma zainstalowane Claude Code i subskrypcję; bez tego dostaje `Unavailable("claude not found on a trusted PATH")`. To racjonalne dla twórcy-developera, ale dla „Obsidian user, który chce nagrywać spotkania" jest to najostrzejszy klif onboardingu. Alternatywy już są w kodzie (Anthropic BYO key, Ollama, lokalny sidecar) — brakuje **decyzji produktowej**, która z nich jest domyślna dla nie-developera, i ścieżki „zero konfiguracji": lokalny brain ma `brain_backend = Cloud` jako default, więc nawet pobrany GGUF nie zostanie użyty bez świadomego przełączenia.

### 9.3 Tempo vs. utrzymywalność

259k linii Rusta bez testów (+79k testów), 70k TS, 360 komend, 93 pola konfiguracji, 54 tabele, 162 komponenty — przy jednym autorze i ~1 300 commitów od czerwca. 44 % commitów między 2.0.0 a 2.3.1 to `fix:`. Nie jest to samo w sobie wadą, ale trzy sygnały mówią, że system testów nie nadąża za powierzchnią: (a) diaryzacja wypadła z produkcji **6 tygodni temu** i żaden test tego nie zauważył, bo oracle e2e testuje inną ścieżkę (R8); (b) `retry_transcription` istnieje po obu stronach IPC, ale nikt go nie woła — dryf „handler bez wywołania" nie ma checku (F2: 16 sztuk); (c) ruleset GitHuba jest wyłączony, więc jedyny zdalny autorytet merge jest dziś doradczy. Te trzy checki są tanie (test ścieżki produkcyjnej ASR, lint „komenda bez wywołania FE", re-arm ruleset) i domykają największe dziury w siatce.

### 9.4 Współbieżność: jedna baza, jeden mutex, dwa wątki UI-krytyczne

`Mutex<Connection>` + WAL + `busy_timeout` 5 s to sensowny wybór dla local-first, ale S1 (238 komend sync na głównym wątku) i O4 (globalny mutex org trzymany przez HTTP i permit inferencji) razem tworzą klasę „aplikacja zamarza, bo coś w tle trzyma bazę". #647 naprawił jeden objaw. Systemowa naprawa to dwie rzeczy: `#[tauri::command(async)]` (lub `spawn_blocking`) dla odczytów listujących/przeszukujących oraz **nietrzymanie żadnego mutexu przez I/O sieciowe lub permit inferencji** — z jednym pomiarem `Db::lock` wait podczas org sync jako oracle'em.

### 9.5 Bezpieczeństwo lokalne vs. zespołowe

Lokalny model bezpieczeństwa (SQLCipher + CK/KEK + bramki na każdym odczycie + MCP z rewalidacją) jest najlepszą częścią aplikacji i wielokrotnie audytowaną. Model zespołowy dziedziczy z niego kryptografię, ale **nie dziedziczy dyscypliny „każda ścieżka ma oracle"**: rotacja bez testu, zaproszenie bez TOFU, aliasowanie blobów bez sprawdzenia własności. Serwer jest solidny w autoryzacji per żądanie, a słaby w przypadkach brzegowych cyklu życia (FK NO ACTION + bezwarunkowe DELETE).

---

## 10. Priorytety napraw

**P0 — przed jakąkolwiek rekomendacją zespołową / przed kolejnym release'em**
1. O1: rotacja OCK — body `{generation}`, granty dla **wszystkich** aktywnych członków (cache kluczy przy zaproszeniu, limit 20 lookupów/dzień), rotować **przed** usunięciem lub journalować `rotation_pending`; test mock-relay asertujący body i pełny zbiór grantów.
2. E1/E2: linki ręczne i fakty **seal-and-restore**, nie purge; oracle round-trip lock→unlock dla `links`+`facts`.
3. R1: przywrócić diaryzację + klasyfikację echa + `estimate_stream_offset` do `run_file_backed_inner` **albo** ukryć cztery martwe przełączniki; e2e ścieżki produkcyjnej (`transcribe_raw_windows` → `merge_streams` → `publish_mix`, profil Accurate).
4. R2: przycisk „Retry transcription" dla meetingów `Error` z audio.
5. S3: re-arm ruleset „Protect" (required check `gate`, no force-push) — bez tego reszta bramek jest doradcza.

**P1 — stabilność i wiarygodność**
6. S1/O4: async dla komend listujących; żaden mutex nie trzymany przez HTTP/inferencję; pomiar `Db::lock` wait.
7. E3/E4: historia Ask scoped do folderów przy delete (jak przy seal), restore z kosza z re-linkowaniem i re-ekstrakcją.
8. O2/O3: `tofu_check` na zaproszeniu; serwer odrzuca legacy `blobId` lub wymaga własnego itemu; `DELETE FROM blobs … WHERE NOT EXISTS (org_items ref)`.
9. S2: DEK — „nie mintuj, gdy istnieje zaszyfrowana baza" + jednorazowy eksport klucza odzyskiwania.
10. B1: poprawić snippet MCP w README/landing (token + format Claude Desktop) i trzy nieaktualne docsy.
11. S4: opt-out i wpis w ledgerze dla pingu aktualizacji.

**P2 — jakość i higiena**
12. R3/R4: okna z zakładką + dedup na granicy; wymuszony wybór języka dla PL w onboardingu.
13. F1: `RENDER_CAP`/wirtualizacja na `/library` i `/notes`; `formatDate` do jednego serwisu.
14. F2/F3: usunąć 16 komend bez wywołania (albo dodać lint) i ~1.7k linii martwych komponentów; `reminder_runtime_probe_control` pod `cfg(debug_assertions)`.
15. L1: pomiar RSS sidecara przy 30 Ask; jeśli rośnie — respawn co N żądań jako backstop.
16. B2: floor Ask na uchwycie real-only + próg 0.78 jak MCP.
17. S6: log 7 dni + „Save diagnostics bundle"; S11: dev-scoped instance lock.
18. F5: jeden słownik hierarchii + rozszerzyć `check-vocabulary.mjs` o rzeczowniki hierarchii.

---

## Załącznik A — liczby

| Metryka | Wartość |
|---|---|
| Rust: linie bez testów / z testami | 259 155 / 79 214 |
| Komendy Tauri (sync / async) | 352 (238 / 114); 355 zarejestrowanych, dryf FE→Rust 0 |
| Metody IPC w FE | 340 |
| Tabele SQLite / linie `migrate()` | 54 / 1 152 |
| Pola `AppConfig` | 93 |
| Narzędzia MCP | 20 |
| Testy Rust / ignorowane (lokalnie) | 3 593 / 18 — **3 575 passed, 0 failed** (202 s) |
| E2E: pliki / testy | 116 / 499 × chromium+webkit |
| Komponenty FE / linie TS / HTML | 162 / 69 692 / 25 761 |
| Miejsca paniki w kodzie shipowanym | ~25, wszystkie dowodliwie niezawodne |
| `#[allow(too_many_arguments)]` | 119 |
| Commity 2026-06 / 07 / 08 | 319 / 924 / 417 |
| Release 2.3.1 idle RSS po 26 h | 127 MB |
| FE po 40× „Ask Brain" w edytorze | 1373 → 1373 węzłów, 101 → 101 listenerów, 19.6 → 19.6 MB |

## Załącznik B — czego ta analiza nie dowodzi

Touch ID / lock-at-rest / auto-relock przy screen-share (tylko podpisany build), jakość transkrypcji PL na Metalu, RSS sidecara przy wielokrotnym Ask, zachowanie przy odpięciu urządzenia audio, dwuklientowy round-trip Shared Brain na żywo, aktualny recall retrievalu (ostatni pomiar 2026-07-10).
