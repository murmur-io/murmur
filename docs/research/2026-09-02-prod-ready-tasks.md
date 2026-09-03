# Murmur → prod ready — lista zadań

**Źródło:** `docs/research/2026-09-02-full-app-analysis.md` (audyt 2.3.1, trunk `392fd223`).
**Żywy stan wykonania:** `../.murmur-agent-tasks/prod-ready/STATUS.md` (poza repo — pętla aktualizuje go bez commitów; ten plik dostaje finalne statusy w zamykającym PR).

Konwencje obowiązujące każde zadanie (z `CLAUDE.md`, nie powtarzane niżej):
- izolowany worktree `git worktree add -b <slug> ../.murmur-agent-tasks/<slug> origin/murmur`; **nigdy** `checkout -b` w primary checkout;
- zadania oznaczone **[h]** idą przez `scripts/h run <id> --prompt "…"` (dotykają lock/crypto/egress/protocol → weryfikuje inny model); pozostałe: worktree + `adversarial-verifier` (na innym modelu niż implementujący) + `lock-security-reviewer` tam, gdzie wskazano;
- bugfix wymaga testu **RED na starym kodzie, GREEN na nowym**; feature wymaga oracle'a;
- bramki: `.agents/h/mirror-check` → `(cd src-tauri && cargo test --lib)` → `npx ng lint` → `npx ng build`; e2e dla zmian FE;
- PR do `murmur` (`gh pr create -R murmur-io/murmur`), merge **po zielonym CI** (`gh pr merge --merge`); CI red → kolejny commit na tej samej gałęzi, nigdy nowe id;
- tożsamość commitów: taka, jaką repo ma w `git config` / CLAUDE.md w chwili commitu; bez trailerów AI;
- po merge: `git worktree remove`, wpis w STATUS.md.

Kolejność = priorytet + zależności. Zadanie „blocked" nie zatrzymuje pętli — pętla idzie dalej i wraca na końcu.

---

## P0 — blokery (przed jakąkolwiek rekomendacją zespołową i przed kolejnym release'em)

### T00 · docs · Wgraj audyt i tę listę na trunk
- **Zakres:** `docs/research/2026-09-02-full-app-analysis.md`, `docs/research/2026-09-02-prod-ready-tasks.md`, wpis w `docs/README.md`.
- **Akceptacja:** PR docs-only, CI zielone, merge.
- **Ścieżka:** worktree, bez reviewera.

### T01 · P0 · [h] Rotacja klucza org kończy się i jest we właściwej kolejności (O1)
- **Problem:** `share/client.rs:1100-1116` POST-uje `/v1/orgs/{id}/generation` bez body → axum 415; `commands/org.rs:3708-3753` przygotowuje grant tylko dla ownera, serwer wymaga każdego aktywnego członka; rotacja dzieje się **po** usunięciu członka. Żaden test nie woła `org_remove_member_inner`.
- **Zakres:** `src-tauri/src/share/client.rs`, `src-tauri/src/commands/org.rs` (remove-member + bump-generation), cache kluczy członków (przy zaproszeniu / liście członków; limit `KEY_LOOKUPS_PER_DAY = 20` vs `MAX_ORG_MEMBERS = 50`), journal `rotation_pending` w `storage/org_store.rs` jeśli rotacja nie może się domknąć w jednym przebiegu.
- **Akceptacja (RED→GREEN):** test mock-relay asertujący (a) `Content-Type: application/json` + body `{generation}` na `/generation`, (b) `PUT /key-grants` pokrywa **każdego** aktywnego członka poza usuwanym, (c) kolejność: grants gen N+1 → bump → dopiero remove (albo remove + `rotation_pending` re-drivable po restarcie); test `org_remove_member_inner` end-to-end na mock relay; istniejące 650 lifecycle zielone.
- **Reviewer:** `lock-security-reviewer` + weryfikator harnessu.
- **Zależności:** —.

### T02 · P0 · [h] Linki ręczne przeżywają lock→unlock (E1)
- **Problem:** `storage/links.rs:1640-1642` `LINK_DECISION_KEEP` zostawia tylko `dismissed` lub `active AND created_by='accepted'`; link z `link_items` (`created_by='user'`) nie ma markera w treści (`commands/links.rs:283-323`), `rederive_links_for_folder` (`commands/mod.rs:5871-5978`) go nie odtwarza. Test `purge_links_tx_drops_manual_on_seal` (`db_tests/tests.rs:13069`) utrwala utratę.
- **Zakres:** `LINK_DECISION_KEEP` → zachować `created_by='user'` (linki to same id, nie treść — ale oba końce muszą pozostać niewidoczne, gdy którykolwiek jest zapieczętowany: wykorzystać istniejące bramki obu końców z `commands/links.rs:304-306`); alternatywnie seal-and-restore linków ręcznych pod CK. Poprawić stary komentarz w `mod.rs:5963`. Odwrócić sens testu.
- **Akceptacja:** nowy oracle `lifecycle_round_trip_manual_links` w `storage/db_tests/lock_tests.rs`: lock → (link niewidoczny w Related/graph/Ask przez `*_visible`) → unlock → zbiór krawędzi identyczny jak przed; `commands/tests/lock_read_gate_tests.rs` nadal zielone.
- **Reviewer:** `lock-security-reviewer` (wymagany) + weryfikator harnessu.

### T03 · P0 · [h] Ledger faktów przeżywa lock→unlock (E2)
- **Problem:** `storage/seal_store.rs:596-612` purguje `facts`, `user_facts`, `supersessions`, korekty, voiceprinty przy seal; unlock (`commands/lock.rs:1056,1190,1752`) re-derivuje tylko linki.
- **Zakres:** seal-and-restore faktów pod CK (wzorzec `seal_note`/`seal_timeline`: blob + verify-before-destroy → blank; unseal przywraca wiersze); rollupy/scores nadal purge (pochodne, regenerowane co godzinę).
- **Akceptacja:** oracle `lifecycle_round_trip_facts` (lock → `list_facts_visible` puste → unlock → identyczny zbiór faktów z zachowanymi `valid_from/valid_to`); `knowledge_diff`/dossier po unlocku mają historię; round-trip byte-identical dla blobów.
- **Reviewer:** `lock-security-reviewer` (wymagany) + weryfikator harnessu.
- **Zależności:** T02 (ten sam obszar `seal_store`/`lock.rs` — po kolei, nie równolegle).

### T04 · P0 · Diaryzacja / echo / offset wracają na ścieżkę produkcyjną **albo** przełączniki są uczciwe (R1 + R8)
- **Problem:** `pipeline.rs:1070-1078` (`run_file_backed_inner`) zwraca `merge_streams(streams)`; cała logika (`Diarizer::load`, `relabel_others`, `classify_cross_stream_echo`, `estimate_stream_offset`, `cancel_echo_offline`) żyje w `run_inner`, osiągalnym tylko z legacy salvage (`audio/spill.rs:2151`). Od `429e83f0` (2026-07-23). Cztery przełączniki (`diarize_others`, `voiceprint_enabled`, `aec_enabled`, `post_aec_enabled`) są martwe.
- **Zakres:** przeczytać uzasadnienie w `429e83f0` (RAM/hang z lipca). Minimum: `estimate_stream_offset` + `classify_cross_stream_echo` (bez modelu, tanie) na ścieżce file-backed; diaryzacja pod istniejącym przełącznikiem z ograniczeniem (bounded, pod watchdogiem ASR i heavy-permitem). Jeśli diaryzacja nie mieści się w budżecie RAM/czasu — **ukryć/wyłączyć** martwe przełączniki z uczciwą kopią i wpisem w release notes. Dodać e2e ścieżki produkcyjnej do `scripts/e2e-mix.sh`: `transcribe_raw_windows` → `merge_streams` → `publish_mix`, profil Accurate.
- **Akceptacja:** test RED na starym kodzie: dual-stream fixture z echem → dziś duplikaty `me`; po zmianie duplikaty sklasyfikowane; e2e produkcyjne zielone w CI; RAM podczas Stop nie rośnie ponad obecny poziom (pomiar `scripts/measure-recording-ram.sh` jeśli dostępny Mac).
- **Reviewer:** `adversarial-verifier` (na innym modelu) + konsultacja `model-perf-engineer`.
- **Uwaga:** nigdy nie throttlować archiwum; pętla live bez zmian.

### T05 · P0 · „Retry transcription" istnieje w UI (R2)
- **Problem:** `retry_transcription` (`commands/mod.rs:7555-7680`) nie ma wywołania w FE (tylko `ipc.service.ts:1259`); watchdog i terminal guard każą go użyć.
- **Zakres:** akcja w widoku meetingu (`features/detail`) i w wierszu listy (`features/library`) dla statusu `Error` z `audio_path`; stan pending + toast; zachować surowe strumienie inflight do czasu trwałej notatki, żeby retry był dual-stream (sprawdzić `cleanup_completed_archived_generation`).
- **Akceptacja:** e2e: meeting `Error` z audio → przycisk widoczny → klik → `retry_transcription` wywołane z właściwym id → status przechodzi w `Transcribing`; meeting bez audio → brak przycisku; test Rust na retry z archiwum dwustrumieniowego.
- **Reviewer:** `adversarial-verifier`.

### T06 · P0 · GitHub egzekwuje bramkę: ruleset „Protect" re-armowany, lane dependabota zielone (S3)
- **Problem:** ruleset `Protect` = `enforcement: disabled`, classic protection 404; `remote harness boundary` w CI pada na każdym PR dependabota (brak `MURMUR_SERVER_DEPLOY_KEY` dla `dependabot[bot]`).
- **Zakres:** (a) `gh api` — włączyć ruleset z: PR wymagany, required status check `gate (full ci.sh — release parity)`, no force-push, no deletion, bypass dla admina (solo-friendly jak w 2026-07-03); (b) `.github/workflows/ci.yml` — dla `github.actor == 'dependabot[bot]'` pominąć krok wymagający deploy-key (lub dodać Dependabot secret) tak, by audyt zdalny nie był czerwony z definicji.
- **Akceptacja:** `gh api repos/murmur-io/murmur/rulesets` pokazuje `active`; następny PR dependabota zielony; audyt `remote harness boundary` na trunku PASS.
- **Reviewer:** `ci-cd-engineer`. **Operator:** właściciel zatwierdził mutację ustawień zdalnych w tej pętli.

## P1 — stabilność i wiarygodność

### T07 · P1 · Odczyty listujące/przeszukujące nie blokują głównego wątku (S1)
- **Problem:** 238/352 komend to sync `fn` na głównym wątku z jednym `Mutex<Connection>`.
- **Zakres:** `#[tauri::command(async)]` lub `spawn_blocking` dla co najmniej: `list_meetings`, `get_meeting_detail`, `get_meeting_segments`, `search_meetings`, `get_graph`, `get_full_graph`, `get_analytics`, `list_notes`, `list_notes_typed`, `list_workspace_tree`, `list_documents`, `list_dashboards`, `get_dashboard`, `export_*`; każda zachowuje swoją bramkę (`meeting_is_unlocked`/`visibility_clause`).
- **Akceptacja:** pomiar przed/po: `Db::lock` wait podczas `delete_meeting` z kaskadą / org sync (tracing span `target:"db"` z czasem oczekiwania, bez PII); `lock_read_gate_tests` zielone; brak nowych `await_holding_lock`.
- **Reviewer:** `app-perf-engineer` implementuje / `adversarial-verifier` + `lock-security-reviewer` (bramki nie mogą się przesunąć).

### T08 · P1 · [h] Żaden mutex org nie jest trzymany przez HTTP ani permit inferencji (O4)
- **Problem:** `org_share_mutation_lock` (`state.rs:352`) ~45 holderów bez limitu; tick 60 s trzyma go przez 4×30 s HTTP i permit `heavy_inference` (`commands/org.rs:9270, 10506`); `unlock_folder` trzyma go przez odczyt Touch ID (`commands/lock.rs:825,874-877`).
- **Zakres:** lock per zatwierdzona akcja (take → commit → release), nigdy przez I/O; wszystkie holdery bounded (`share-busy`); `unlock_folder` bierze lock dopiero po KEK.
- **Akceptacja:** test: tick z zawieszonym HTTP nie blokuje `move_note`/`start_recording`; test kolejności `unlock_folder`; 11 oracle'i z #647 nadal zielone.
- **Reviewer:** `lock-security-reviewer` + weryfikator harnessu.
- **Zależności:** T01 (ten sam plik `commands/org.rs`).

### T09 · P1 · [h] Historia Ask ginie tylko w zakresie usuwanego elementu (E3)
- **Problem:** `purge_all_ask_conversations_tx` w `db.rs:3847, 4569, 5661, 6029, 7322` + `facts_store.rs:615,643,662`, `reminder_store.rs:977` — kosz jednego meetingu kasuje każdą rozmowę.
- **Zakres:** scoped purge po zbiorze folderów (jak przy seal, `ask_history.rs` `visible_folder_ids`); globalny purge tylko tam, gdzie nie da się nazwać zbioru; `visibility_generation` bez globalnego bumpa.
- **Akceptacja:** RED: delete meetingu w folderze A → rozmowa zależna tylko od folderu B znika; GREEN: zostaje; rozmowy zależne od A znikają; brak wycieku (seal testy zielone).
- **Reviewer:** `lock-security-reviewer` + weryfikator harnessu.

### T10 · P1 · Restore z kosza przywraca linki, wzmianki i fakty (E4)
- **Problem:** `storage/trash_store.rs` snapshotuje tylko wiersz/segmenty/notatki/timeline/tagi; delete purguje linki (`preserve=false`), kaskaduje `entity_mentions`, kasuje fakty; `commands/trash.rs` restore nie re-linkuje.
- **Zakres:** rozszerzyć snapshot o `links` (oba kierunki), `entity_mentions`, `facts`/`supersessions` danego meetingu; restore odtwarza je deterministycznie (bez wywołań cloud); potem `index_wikilinks` + companion.
- **Akceptacja:** RED→GREEN: trash → restore → zbiór linków/wzmianek/faktów identyczny; purge po retencji nadal kasuje wszystko; sealed items pomijane jak dziś.
- **Reviewer:** `lock-security-reviewer` (trash × seal) + `adversarial-verifier`.
- **Zależności:** T02, T03 (kształt seal linków/faktów).

### T11 · P1 · [h] Zaproszenie do org pinuje klucz (O2)
- **Problem:** `commands/org.rs:3567-3596` owija OCK dowolnym `pk_enc` z `lookup_key`, bez `tofu_check` (`commands/mod.rs:14282`, używany w mode-B `org.rs:1493-1510`).
- **Zakres:** TOFU na kluczu zapraszanego (pierwsze użycie zapisane, zmiana → odmowa + jasny błąd), granter pinowany pod stabilnym id ownera po stronie grantee (`org.rs:2728-2747`).
- **Akceptacja:** test: relay podmienia `pk_enc` po pierwszym zaproszeniu → `AppError::Auth`/tagged code, grant nie wychodzi; happy path bez zmian.
- **Reviewer:** `lock-security-reviewer` + weryfikator harnessu.
- **Zależności:** T01.

### T12 · P1 · Serwer: legacy `blobId` nie może aliasować cudzych blobów; kasowanie blobów jest bezpieczne (O3 + O5-serwer)
- **Repo:** `../murmur-server` (`crates/murmur-server/src/routes/orgs.rs:424-441`, `store/shares.rs:485`, `store/orgs.rs:468, 2149`, `store/accounts.rs:466`).
- **Zakres:** odrzucić `blobId` w publish (albo wymagać własnego itemu w tej org); `DELETE FROM blobs … WHERE NOT EXISTS (SELECT 1 FROM org_items WHERE blob_id = blobs.id)`; legacy POST bez `docId` egzekwuje członkostwo/generację w tx; testy w `tests/orgs.rs`.
- **Akceptacja:** testy serwera: (a) współczłonek aliasujący cudzy blob → 4xx, (b) revoke/org-delete/erase ofiary → 200 mimo aliasu; CI serwera zielone; deploy przez skill `deploy-murmur-server`; bump `.murmur-server-revision` w kliencie osobnym PR.
- **Reviewer:** `adversarial-verifier`. **Deploy:** wykonać po zielonym CI serwera; wpisać nową rewizję do STATUS.md.

### T13 · P1 · [h] DEK nie jest mintowany, gdy istnieje zaszyfrowana baza + eksport klucza odzyskiwania (S2)
- **Problem:** `secrets/keychain.rs:81-150` odmawia mintu tylko przy `-34018`; istniejąca baza + „absent" DEK → nowy DEK → baza nieczytelna. KEK ma guard (`keychain.rs:206, 760`), DEK nie.
- **Zakres:** guard „plik `meetnotes.sqlite` istnieje i nie jest plaintext → nie mintuj, pokaż dialog odzyskiwania"; Settings → Privacy: jednorazowy „Export recovery key" (odczyt DEK za user-presence, zapis do pliku wskazanego przez użytkownika, wpis w ledgerze lokalnym), „Restore from recovery key" na ekranie błędu startu.
- **Akceptacja:** test: DB istnieje + keychain zwraca absent → `Err(Secrets)` bez mintu; happy fresh-install mintuje; round-trip export→restore otwiera bazę. **Nigdy nie logować klucza.** Dev hatch `MURMUR_DEV_DEK` bez zmian.
- **Reviewer:** `lock-security-reviewer` (wymagany) + weryfikator harnessu.

### T14 · P1 · Dokumentacja MCP zgodna z serwerem (B1 + B7)
- **Zakres:** `README.md:528-533`, `landing/docs.html:871-877` — snippet z tokenem w formacie, który akceptują Claude Code (`type: http` + `headers.Authorization`), Claude Desktop (Custom Connector / mostek stdio) i Codex; poprawić `docs/ARCHITECTURE-LOCAL-CLOUD.md:13,46`, `docs/USE-WITH-YOUR-AGENT.md:37`, `docs/PHASE0-PLAN.md:81`; snippet ma być identyczny z tym, co generuje `commands/mod.rs:6825-6850`.
- **Akceptacja:** test Rust, że wygenerowany config zawiera bearer; skill `sync-release-copy` przechodzi bez findingów dla MCP.
- **Reviewer:** `adversarial-verifier` (docs vs kod).

### T15 · P1 · Ping aktualizacji za zgodą i w ledgerze (S4)
- **Problem:** `app.component.ts:136` → `update.rs:123-162` bije w `api.github.com` przy każdym starcie bez opt-out i ledgera.
- **Zakres:** ustawienie `update_check_enabled` (domyślnie ON, widoczne w onboardingu przy providerze i w Settings → Privacy), wpis w egress ledger (bez treści), ręczne „Check now" niezależne.
- **Akceptacja:** test: flaga OFF → zero requestów; ON → wpis w ledgerze; e2e Settings.
- **Reviewer:** `adversarial-verifier`.

### T16 · P1 · Warstwa zespołowa nie udaje sukcesu (S5)
- **Zakres:** `shared-workspace.service.ts:152-154` — błąd `listSharedWorkspace`/`listContainerShareStatus`/`listOrgShareTargets` → widoczny stan błędu (banner + retry), nie pusty workspace; `settings-account-section.component.ts:207` rewrap → toast danger; `note-editor.component.ts:1367,1383`, `note-panel.component.ts:367,379` cleanup załączników → log + toast; przegląd 25 `catch(() => …)` i 5 `void this.ipc.*` — każdy albo uzasadniony komentarzem „best-effort", albo naprawiony.
- **Akceptacja:** e2e: mock rzuca na `list_shared_workspace` → banner widoczny, brak „pustego" stanu; lint nie regresuje.
- **Reviewer:** `adversarial-verifier`.

### T17 · P1 · Floor Ask używa tylko prawdziwego embeddera z progiem (B2)
- **Zakres:** `commands/ask.rs:355-361, 566-577` → uchwyt real-only (jak `tools.rs:754-868`), `vault_context.rs:197, 272` → `KNN_SEARCH_COSINE_FLOOR` zamiast `0.0`; bez modelu noga KNN pusta i redystrybuowana.
- **Akceptacja:** test: stub embedder → `search_hybrid_visible` wołane bez wektora; z modelem → próg 0.78; bake-off syntetyczny bez regresji (`eval/results/rag-bakeoff-latest.md`).
- **Reviewer:** `adversarial-verifier` (+ `memory-retrieval-architect` konsultacja).

### T18 · P1 · Flaky test na trunku naprawiony (S7)
- **Problem:** `commands::lifecycle_tests::collaborator_advanced_head_is_terminal_even_when_head_scan_is_unavailable` (`lifecycle_tests.rs:30364`) padł na CI dla #647, przeszedł w kolejnym runie.
- **Zakres:** znaleźć wyścig (mock relay / TMPDIR / port), naprawić deterministycznie; uruchomić 20× lokalnie.
- **Akceptacja:** 20/20 lokalnie i zielone CI.
- **Reviewer:** `adversarial-verifier`.

## P2 — jakość i higiena

### T19 · P2 · Okna dekodowania z zakładką i stabilny język (R3 + R4)
- **Zakres:** `pipeline.rs:1160-1166, 1394-1412` — zakładka (np. 5 s) + dedup na granicy (timestamp/tekst); język: wynik detekcji z pierwszego regionu pinowany na resztę nagrania, chyba że użytkownik ustawił język; onboarding: wybór języka wyeksponowany dla PL.
- **Akceptacja:** test `decode_windows` z zakładką i dedupem; test pinowania języka; e2e onboarding.
- **Reviewer:** `adversarial-verifier` + `model-perf-engineer` (koszt zakładki).

### T20 · P2 · Watchdog ASR zwalnia maszynę; UI wie, że system-audio umarło (R5 + R6)
- **Zakres:** po watchdogu zwolnić heavy-permit/lease (albo oznaczyć generację jako martwą, żeby następny Start nie odmawiał); `RecordingStatus` (`commands/audio.rs:31-43`) niesie `systemCaptureAlive: bool` + powód; FE pokazuje ostrzeżenie w trakcie nagrania.
- **Akceptacja:** test: po watchdogu `start_recording` przechodzi; e2e: status z `systemCaptureAlive=false` → banner.
- **Reviewer:** `adversarial-verifier`.

### T21 · P2 · `/library` i `/notes` renderują z oknem (F1)
- **Zakres:** `RENDER_CAP` + „pokaż więcej"/wirtualizacja bez nowych zależności; jeden `DateFormatService` zamiast 10 kopii `formatDate()`; `list_meetings` z limitem/paginacją albo lekki DTO listy.
- **Akceptacja:** e2e z 1 000 wierszy w mocku: czas do interakcji i liczba węzłów DOM ograniczone; brak regresji w 499 e2e.
- **Reviewer:** `adversarial-verifier` (WebKit).

### T22 · P2 · Martwy kod i martwe komendy (F2 + F3 + S10)
- **Zakres:** 16 komend bez wywołania w FE — podłączyć (zarządzanie serwerami MCP w Settings) albo usunąć; `reminder_runtime_probe_control` pod `cfg(debug_assertions)`; usunąć `DashboardTileComponent`, `AiOrbComponent`, `MurCardComponent`, `MurInputComponent` (albo użyć); skrypt CI „zarejestrowana komenda bez wywołania w `ipc.service.ts`" jako lint w `scripts/ci.sh` (skill `add-ci-gate`).
- **Akceptacja:** lint RED na obecnym trunku, GREEN po zmianie.
- **Reviewer:** `adversarial-verifier`.

### T23 · P2 · Diagnostyka: 7 dni logów + „Save diagnostics bundle"; dev-scoped instance lock (S6 + S11)
- **Zakres:** `applog.rs:49` retencja 7 dni z limitem rozmiaru; komenda `export_diagnostics_bundle` (logi + `app_info` + wersje modeli, **bez PII**) z przyciskiem w Developer; `instance_lock.rs:49` → ścieżka przez `state::app_dir_name()`.
- **Akceptacja:** test retencji; test, że bundle nie zawiera tytułów/transkryptów (grep fixture); dev-app startuje obok release'u.
- **Reviewer:** `adversarial-verifier`.

### T24 · P2 · Jeden słownik hierarchii (F5)
- **Zakres:** decyzja: *Workspace* › *folder* › element (albo *Space* — jedno słowo wszędzie); poprawić toasty/błędy w `workspace-tree`, `container-view`, `shared-container-view`, „People & projects"; rozszerzyć `scripts/check-vocabulary.mjs` o rzeczowniki hierarchii w trybie strict dla nowych stringów.
- **Akceptacja:** vocabulary gate GREEN; e2e stringów.
- **Reviewer:** `adversarial-verifier`.

### T25 · P2 · Pułapki FE: NG0911, guardy kolejności, §8 (L5 + F4 + F7)
- **Zakres:** `onboarding.component.ts:294-301`, `brain-enable-card.component.ts:96-103`, `settings.store.ts:1840-1881` → wzorzec z `record.component.ts:470-499`; `graph.component.ts:169`, `people.component.ts:99` → seq guard + §8 (cache nie chowany za `loading()`); NUL w `entity-detail.component.ts:240` → `" "`; `standalone: true` w `stage2-panel`.
- **Akceptacja:** e2e: nawigacja w trakcie pobierania modelu nie rzuca NG0911; relock→unlock na `/graph` nie pokazuje starego zbioru.
- **Reviewer:** `adversarial-verifier`.

### T26 · P2 · Pomiar RSS sidecara przy 30 Ask (L1) — **wymaga Maca i zamkniętego release'u**
- **Zakres:** przy uruchomionym lokalnym brainie: `ps -o rss= -p $(pgrep -x meetnotes-brain)` co 5 s przez 1 → 10 → 30 zapytań; jeśli monotoniczny wzrost bez resetu — backstop „respawn co N żądań" w `reason/sidecar.rs` (tylko po pomiarze, nigdy na zapas).
- **Akceptacja:** liczby w `docs/research/`; decyzja z pomiaru.
- **Status:** `blocked-on-user`, dopóki właściciel nie zamknie zainstalowanego Murmura albo nie odpali pomiaru sam.

### T27 · P2 · Bump pinu serwera + kopia release'owa (po T12)
- **Zakres:** `.murmur-server-revision` → nowa rewizja; `sync-release-copy` dla landing/README (MCP, uprawnienia org, retry).
- **Reviewer:** `adversarial-verifier`.

### T28 · final · Release 2.4.0 — **tylko na wyraźne „go" właściciela**
- **Zakres:** skill `release-murmur` (gates → bump → PR → build universal → sign inside-out → DMG → notarize → staple → `gh release`). Release notes = diff `v2.3.1..v2.4.0` z sekcją „co było zepsute" (diaryzacja, retry, rotacja).
- **Akceptacja:** `spctl -a -vvv -t open --context context:primary-signature` = Notarized Developer ID.

---

## Prompt pętli (kopia; żywa wersja w STATUS.md)

Zobacz sekcję „Prompt" w `../.murmur-agent-tasks/prod-ready/STATUS.md`.
