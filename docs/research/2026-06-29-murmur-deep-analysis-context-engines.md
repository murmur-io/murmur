# Murmur — co realnie mamy, jak wypadamy vs front „context/memory engines", i co przejąć

## 1. TL;DR — czym Murmur JEST dziś (nie wg docs v0.3.0)

1. **Kod jest o ~2 generacje dalej niż CLAUDE.md (v0.3.0).** To realnie brain2 Phase 4/5b: hybrydowy retrieval (FTS5 ∪ vec0 ∪ entity-graph z RRF), cross-meeting dossier, redaction firewall z NER-seamem, realtime asystent głosowy z connectorami i lokalny serwer MCP. Większość tego jest **wpięta i headless-przetestowana** — ale kluczowe „mózgi" leżą **za wyłączonymi domyślnie feature-flagami**.

2. **Silnik RAG jest dojrzały, ale bez mózgu w wydaniu.** Cała mechanika (vec0 KNN + FTS5 + RRF + GraphRAG-lite + gating + purge-on-lock) działa w każdym buildzie i jest otestowana ✅ — ale jedynym embedderem w buildzie release jest **deterministyczny hash-bag `StubEmbedder` (NIE semantyczny)**; prawdziwy multilingual-e5 to compile-proven szkielet za `--features local-embed`, którego **release nie kompiluje**, a `semantic_search_enabled` jest domyślnie OFF. Czyli „semantyczny RAG" w wydanym buildzie = leksykalny FTS + stub-wektory.

3. **On-device naprawdę działa tylko w transkrypcji.** whisper.cpp large-v3 (Fast+Accurate) i Silero VAD są zawsze wpięte ✅. Diaryzacja sherpa działa, ale jest opt-in (OFF). Lokalny brain (Bielik/Qwen GGUF), lokalny embedder (e5) i lokalny NER (mDeBERTa) to **trzy szkielety za OFF-flagami** — w produkcji ich miejsce zajmują: **CloudReasoner** (domyślny mózg, chmura+consent), **StubEmbedder** (niesemantyczny) i **NoopNameRedactor** (imiona/nazwiska egresują do chmury bez zmian).

4. **Model prywatności jest mocny i spójny — to realny moat.** Dwie warstwy szyfrowania (SQLCipher DEK + per-folder CK/KEK biometryczny), AAD context-binding (anti-swap), verify-before-destroy na każdej ścieżce seala, podwójne bramkowanie odczytów (`meeting_is_unlocked` + `visibility_clause`), zamknięty leak `convertFileSrc`. To weryfikacja statyczna kodu + testów `--lib`, nie dowód na podpisanym buildzie.

5. **MCP to architektonicznie inny zwierz niż u konkurencji: lokalny, read-only, zero-egress, loopback-only, wizyjnie bramkowany.** 6 narzędzi nad on-device SQLCipher, Host/Origin allowlist, bearer fail-closed. Cała chmurowa konkurencja (Granola/Otter/Fireflies/Fathom/tl;dv/Circleback) ma MCP — ale ich serwery to OAuth do danych już leżących w ich AWS.

6. **Realtime asystent głosowy jest w pełni podłączony end-to-end**, ale **domyślnie uśpiony**: `realtime_reactions=false` → wake tylko surface'uje; web search wymaga enable+consent+BYO Brave key; Slack to stub. Manualne „Ask AI" działa zawsze podczas nagrania.

7. **Jednozdaniowa odpowiedź na pytanie założyciela:** **Nie — nie istnieje narzędzie, które jednocześnie buduje wyraźnie potężniejszy/bardziej oryginalny kontekst, jest MCP-server-native I jest local-first + owned-markdown.** Pojedyncze osie biją Murmura (Pieces — surowy wolumen; NotebookLM — okno 1M tok; Graphiti/Zep/mem0 — warstwa faktów i czasu), ale każdy z nich przegrywa co najmniej jedną oś, którą Murmur trzyma. Murmur jest w 3-elementowej lidze (Pieces, Basic Memory, Murmur), a jako jedyny łączy diaryzowany głos + entity-graph + owned-md + lokalny MCP + lock model.

---

## 2. Pełna analiza obecnej apki

### 2.1 Wektorowa baza danych / semantyczny RAG

**Infrastruktura — ✅ wired w każdym buildzie, headless-tested:**
- `sqlite-vec 0.1.9` to **twarda** zależność (`Cargo.toml:63`), więc vec0 KNN jest w KAŻDYM buildzie. Rejestracja `sqlite3_auto_extension` przez `Once` **przed** `Connection::open` w `open_with_key` (`db.rs:109-128,150`) — bez naruszania reguły „`PRAGMA key` pierwszy".
- Schemat: plaintext `note_chunks` + wirtualna `vec_chunks USING vec0(... embedding float[384])` (`migrate_vector` `db.rs:343-367`); **EMBED_DIM=384** celowo = hidden_size multilingual-e5-small, więc realny model wchodzi bez migracji (`embed.rs:28,78-81`).
- FTS5: trzy external-content tabele (`fts_meetings/_segments/_notes`) z triggerami, tokenizer `unicode61 remove_diacritics 2` — **Polish-aware** (`migrate_fts` `db.rs:387-455`).
- Retrieval to realny **RRF (k=60) nad TRZEMA listami**: FTS5/BM25 + wektor + **entity-graph neighbourhood** = GraphRAG-lite (`search_hybrid_visible` `db.rs:1007-1062`, `rrf_fuse` `embed.rs:245-259`). Zapytanie nazywające encję wciąga jej cross-meeting sąsiedztwo — czego flat-RAG nie ma.
- Gating spójny z lockiem: każdy odczyt wektorowy przez `visibility_clause`; `purge_chunks_for_meetings` w tej samej transakcji co blankowanie przy seal/relock (`db.rs:876-912`); osobny purge w `delete_meeting` (vec0 nie łapie CASCADE — test `vec_chunks_purged_on_delete_meeting` `db.rs:4252`).

**Mózg — 🔴 stub w release / 🟡 gated:**
- Domyślny embedder = **`StubEmbedder`**: FNV-1a hash-bag, L2-norm, kontrakt = TYLKO determinizm, NIE sens semantyczny (`embed.rs:109-123,176-186`).
- Realny **`CandleBertEmbedder`** (e5, candle, Metal) jest **compile-proven only** za `--features local-embed` (OFF by default, `Cargo.toml:138-151`); nagłówek pliku szczerze: „cargo test --lib NEVER runs a forward pass" (`candle_bert.rs:5-18`).
- **`semantic_search_enabled` = false domyślnie** (`config.rs:223`).
- **Release nie kompiluje `local-embed`** (`npx tauri build ... --bundles app`, bez `--features`) → w wydaniu candle jest wycięty, więc nawet po włączeniu flagi i pobraniu modelu to **wciąż stub-wektory**.
- **Footgun guardu reindexu:** `reindex_embeddings` bramkuje na `embed_model_present()` (obecność 3 plików), nie na typie aktywnego embeddera (`commands.rs:2492-2493`) → w buildzie bez feature może zaindeksować stub-wektory mimo doc „no stub indexing".

**Luki vs prawdziwy „huge context":** prompt pakuje **CAŁY markdown notatki** truncowany budżetem (200k znaków ≈ 50k tok dla Claude), a nie wyszukane chunki — brak chunk-level retrieval do promptu, brak rerankera (`vault_context.rs:111-152`). Embedowana jest **tylko notatka, nie transkrypt** (`source_type` zahardkodowane `'voice'`). Always-on grounding nowych notatek (`related_context.rs`) jest **FTS-only, bez wektorów** (`related_context.rs:5-8`).

**Ocena: silnik dojrzały i poprawnie zalockowany; brak realnego embeddera w wydaniu.**

### 2.2 Modele AI w fazach rozmowy i transkrypcji

**Faza LIVE (w trakcie nagrania) — ✅:** pętla `transcribe/live.rs` (osobny wątek, tick 3 s, okno 14 s): captions whisper **Fast** (greedy, `whisper.rs:223`, `live.rs:154-158`); wake-word „Klaudku…" + deterministyczny parser intencji PL/EN; manualny voice-command click-to-stop z wymuszeniem języka klipu (3 s Whisper myli PL→RU).

**Faza POST-MEETING (po Stop) — ✅:** dual-stream 16 kHz (mic + opcjonalny system audio z sidecara ScreenCaptureKit), transkrypcja **Accurate** (beam-5, drabina temperatur, bramki anti-halucynacyjne — kluczowe dla fleksyjnego polskiego, `whisper.rs:231-249`), VAD-segmentacja, merge po wall-clock → me/others.

| Model | Faza | Lokacja | Stan |
|---|---|---|---|
| whisper.cpp large-v3 Fast | LIVE | on-device Metal | ✅ zawsze |
| whisper.cpp large-v3 Accurate | POST | on-device Metal | ✅ zawsze |
| Silero VAD (ggml, **CPU** celowo) | POST pre-seg | on-device | 🟡 gated `vad_enabled`=**true** (`vad.rs:41-50`) |
| pyannote-seg 3.0 + CAM++ (sherpa ONNX) | POST diaryzacja „others" | on-device | 🟡 gated `diarize_others`=**false** (opt-in) |
| **MistralReasoner** (Bielik 11B/Qwen3 14B/Qwen2.5 3B GGUF) | ENRICH + LIVE dispatch | on-device Metal in-proc | 🔴 gated `local-brain` **OFF**, compile-only; default brain=Cloud → nieaktywny (`reason.rs:278-305`, `mistral.rs:5-16`) |
| **CloudReasoner** | ENRICH + LIVE | **CHMURA** | ✅ **domyślny brain**, consent-gated |
| StubReasoner | podłoga | on-device | ✅ gdy Off/Local-bez-modelu (nie default) |
| **DeBERTa-v2 NER** (candle) | POST firewall (mask PERSON) | on-device | 🔴 gated `local-ner` **OFF**, default Noop |
| Graf encji / timeline | POST | **CHMURA** `provider.complete` | ✅ — **NIE lokalny NER** (`graph.rs:31`) |
| e5-small embedder | ENRICH RAG | on-device | 🔴 gated `local-embed` OFF, default Stub |

**Dwie pułapki pojęciowe do zapamiętania:** (a) graf encji NIE używa lokalnego DeBERTa-NER — encje wyciąga **chmurowy provider**; DeBERTa służy wyłącznie do maskowania nazwisk w firewallu egresu. (b) „me/others" to **atrybucja strumienia capture, nie diaryzacja głosu** (`types.rs:11-15`); diaryzacja sherpa dokłada N-way osobno, tylko na „others".

### 2.3 Blokady notatek i folderów (lock model + prywatność)

**Werdykt: mocny i spójny ✅.** Dwie warstwy: (1) **SQLCipher DEK** na całym pliku, item keychain PLAIN bo czytany przy każdym starcie (`keychain.rs:43-46`); (2) **per-folder CK** (AES-256-GCM) wrapowany **biometrycznym KEK** — keychain `kSecAccessControlUserPresence` + `WhenUnlockedThisDeviceOnly` (`keychain.rs:405-408`), gate **OS-owy, nie app-owy** (flaga `lock_require_biometric` tylko informacyjna, nie da się nią obejść, `commands.rs:3122-3127`).

- **AAD context-binding (mocna strona):** każdy blob AEAD-związany z kontekstem — wrapped-CK→`folder`, content→`folder|meeting|provider|type|v1`, audio→`meeting|folder|stream-role` (`commands.rs:3795,3802,3851`). Swap ciphertextu fails-closed `AppError::Locked` (test `swapped_context_blob_fails_closed` `crypto.rs:239`); legacy puste-AAD bloby nadal się odszyfrują (re-bind), więc brak brickowania.
- **Verify-before-destroy POTWIERDZONE wszędzie:** notatka per-provider decrypt+byte-compare PRZED `seal_note` blankuje markdown (`commands.rs:3060-3066`); transkrypt/timeline weryfikowane przed blank (`:4002,4018`); audio `encrypt_file` asertuje byte-identical w środku przed usunięciem WAV (`crypto.rs:124-131`). Kolejność crash-safe: vault `.md` kasowany NA KOŃCU (`.md` odtwarzalny, treść nie). Startup reconciliation `reblank_locked_folders_at_rest` blankuje tylko rzędy z blobem (`db.rs:1871`).
- **Gating każdego odczytu:** `meeting_is_unlocked` → `masked_detail` (`audio_path=None`, `title="🔒 Locked"`); zerowanie `audio_path` to jedyna rzecz zamykająca leak `asset:`/`convertFileSrc` (test `:4414`).
- **Race-safety:** `lifecycle_guard` Mutex serializuje maszynę stanów, `remove_lock_inner` trzyma guard przez cały restore→clear (`:3331`) — zamyka permanent-loss race; `relock_all_inner` zeroizuje KEK + WAL checkpoint TRUNCATE.

**Luki (wprost):**
1. **Token MCP domyślnie OFF** — gdy OFF, **dowolny lokalny proces** czyta 6 narzędzi bez auth (cała treść ODBLOKOWANYCH spotkań). Łagodzące: loopback + visibility-gated + read-only. Warto rozważyć default ON.
2. **Redakcja nazwisk domyślnie no-op** — w standardowym buildzie **imiona/nazwiska egresują do chmury** (regex łapie tylko email/karty/telefon).
3. `visibility_clause` buduje SQL przez interpolację id folderów (escape `'`→`''`, `db.rs:3061`), nie parametryzowane — smell, póki id wewnętrzne.
4. DEK PLAIN czytany przy starcie → po uruchomieniu cała baza odszyfrowana w sesji; lock-at-rest dla całości działa tylko gdy apka zamknięta (udokumentowany design).

### 2.4 Realtime AI actions (agentic loop + connectory + consent)

**✅ wpięte end-to-end, 🟡 domyślnie uśpione.** Pętla live: wake → intent → gated fan-out (vault FTS + semantic + dossier + web + kalendarz) → JEDNO wywołanie mózgu do syntezy 2-4 zdań → event do FE z cytatami.

- **Dwie różne pętle:** **Flow A** (grounding NOTATKI, `orchestrate.rs`) — reasoner zwraca **plan retrievalu** wg JSON-schemy (max 4 queries) → `execute_tool` → cited corpus; stub-floor `build_grounding_context` byte-identyczny gdy `reasoner.id()=="stub"`. **Flow B** (REALTIME voice, `voice_action.rs`) — **deterministyczny fan-out** (nie planująca pętla LLM): intent → uruchom wszystkie legi → zbierz gated grounding → synteza. Kluczowy fix cross-lingual: queries **literal-first** (PL „pogoda" keyuje na dosłownych słowach, nie na przetłumaczonym topicu, `voice_action.rs:234-248`).
- **Rejestr narzędzi:** 8 wariantów `ToolCall`, jeden bramkowany seam `execute_tool` (`tools.rs:33-61`). 6 vault-tools egress-free (każde przez `*_visible`); 2 connectory (`WebSearch`/`CalendarLookup`) **celowo odrzucane** w synchronicznym `execute_tool` (bo to wejście MCP, MUSI być egress-free) — dispatch tylko async.
- **Connectory:** **web (Brave, BYO key, `EgressClass::External`)** widoczny mózgowi TYLKO gdy `web_search_enabled && web_search_consented && klucz w Keychain` (fail-closed, wszystkie defaulty false); `ConnectorRegistry::search` **redaguje query PRZED** connectorem (`mod.rs:172-173`). **Kalendarz (`EgressClass::Local`, EventKit sidecar)** — świadomie NIE consent-gated, graceful-degrade do pustego Vec. **Slack = stub** („unavailable").
- **Bramki:** `realtime_reactions=false` domyślnie → wake tylko surface'uje (`live.rs:270-272`, `config.rs:227`); manualne „Ask AI" (click-to-stop) działa zawsze. Synteza mózgu = ten sam `make_provider` (consent + RedactingProvider) co notatka.
- **Lock-safety:** każdy realtime read przez `search_visible` na **LIVE `unlocked` set**; testy RED-before-GREEN dowodzą, że sealed-not-unlocked nie wpływa do groundingu/cytowań (`voice_action.rs:846-906`), NoteAside refuse na sealed (`:962-980`).
- **Świeże/uncommitted (handoff layer):** `src/app/shared/assistant-sources.component.ts` (untracked) + refactor `assistant-actions.component.ts`/`detail.component.ts` — czysto prezentacyjny refactor cytowań (dedupe, domain extraction, markdown render), reużywa otestowanego `MarkdownComponent`; **niezweryfikowany bramkami** (brak commita), wymaga `ng lint`/`ng build`.

### 2.5 Serwer MCP (powierzchnia, bezpieczeństwo, rola w budowaniu kontekstu)

**✅ wired, read-only, zero-egress.** 6 narzędzi (`mcp.rs:230-262`, test `tools_list_has_six_tools`): `search_meetings`, `get_meeting`, `list_recent_meetings`, `search_semantic`, `get_open_commitments`, **`get_entity_dossier`**. **Zero write tools.**

- **Transport:** `127.0.0.1:8765` only, Host allowlist `{127.0.0.1:8765, localhost:8765}` (anti DNS-rebinding), Origin allowlist (nigdy nie reflektuje), bearer **fail-closed** (gdy `require_token` a token nie da się zmintować → serwer odmawia startu, nie degraduje do otwartego), porównanie constant-time, body cap 1 MiB, tylko POST.
- **Bramkowanie treści:** każdy `tools/call` → snapshot `unlocked_set` → jeden gated seam `execute_tool`. Sealed-not-unlocked niewidoczne (3 testy visibility-gating).
- **Egress MCP = ZERO:** `execute_tool` czyta tylko lokalne SQLite + lokalny embedder; `WebSearch`/`CalendarLookup` odmawiane synchronicznie — MCP nie sięgnie connectora.
- **Architektoniczny wyróżnik vs cała chmurowa konkurencja:** `get_entity_dossier` przez MCP jest **EGRESS-FREE** — zwraca strukturalne dane do lokalnej syntezy klienta (Claude Desktop), serwer MCP NIGDY nie woła chmury (`mcp.rs:661`, `format_dossier_client` bez provider-calla).
- **Główna luka:** token OFF default (patrz 2.3) — przy OFF każdy lokalny proces ma read-only dostęp do treści odblokowanych spotkań.

---

## 3. Krajobraz konkurencji

Wybór top kandydatów z trzech researchy (meeting-tools + second-brainy + memory-engines):

| Narzędzie | Local-first | Jak buduje kontekst | MCP | Oryginalność/siła vs Murmur |
|---|---|---|---|---|
| **Granola** | ❌ chmura (audio→Deepgram/OpenAI, notatki w AWS) | „Chat with meetings", folder/collection queries, deal-flow DB | ✅ hostowany (Claude/ChatGPT/Cursor) | Większy korpus *dziś* (auto-capture zespołów, $1.5B). Przegrywa: nie local, nie owned-files |
| **Otter.ai** | ❌ chmura | „System of record"; MCP-**klient** wciąga Gmail/Drive/Notion/Salesforce/Jira/Slack | ✅ serwer+klient, OAuth | **Wyprzedza wizję brain2 w breadth** (multi-source dziś). Przegrywa: zero lokalności, walled garden |
| **Fathom / Fireflies / tl;dv / Circleback** | ❌ chmura | „Ask X" ChatGPT-style nad historią, cytowane | ✅ wszystkie | MCP = table-stakes. Żaden nie local, żaden owned-md |
| **Limitless** (ex-Rewind) | ❌ pivot z local → „Confidential Cloud" | „Digital memory", ask-everything, pendant always-on | ✅ MCP URL | **Przejęty przez Meta (gru 2025), Rewind off** — porzucił lokalność |
| **Pieces (LTM-2.7)** | ✅ **w pełni on-device** | **Ambient capture wszystkiego** (kod/ekran/audio), okno **9 mies.** | ✅ **własny serwer** (`ask_pieces_ltm`) | **Potężniejszy w SUROWYM wolumenie** + lokalny + MCP. Przegrywa: pasywne „wysypisko" bez syntezy/encji, closed-source, brak owned-md/lock — **landfill, którego świadomie unikamy** |
| **Basic Memory** | ✅ local-first | **markdown=truth + SQLite-index + graf przez rekurencyjny traversal**; observations/relations | ✅ **MCP-native, dwukierunkowy** (`write_note`/`build_context`) | **Architektoniczny bliźniak brain2 + WRITE-back.** Przegrywa: zero capture (brak głosu), brak embeddingów default, mniejsza skala, brak lock/redaction, AGPL |
| **Zep / Graphiti** | 🟡 da się lokalnie, managed=cloud, wymaga Neo4j | **Bitemporalny knowledge graph** — krawędzie z `t_valid/t_invalid`, **inwalidacja nie kasowanie** | ✅ eksperymentalny | **Najbardziej oryginalny** — odpowiada „stan na teraz" vs „miesiąc temu". Przegrywa: graph-DB + LLM przy ingeście, nie meeting-native |
| **mem0 / OpenMemory** | ✅ OpenMemory MCP lokalny | **Ekstrakcja faktów + reconcile ADD/UPDATE/DELETE/NOOP**; multi-signal fusion (semantic∪BM25∪entity) | ✅ OpenMemory MCP | **Warstwa faktów, której Murmur nie ma** — ale retrieval-fusion to dokładnie to, co Murmur już ma. Przegrywa: nie owned-md, nie meeting-native |
| **Letta (MemGPT)** | ✅ self-host | **Self-editing memory (core/recall/archival) + sleep-time compute** | ✅ kompatybilny | Oryginalny paradygmat „memory-as-OS". Dla nas cenny 1 koncept: idle-konsolidacja |
| **NotebookLM** | ❌ chmura Google | **Największy surowy kontekst: 1M tok / 25M słów**, Deep Research, Audio Overview | ❌ brak | Najbigger okno + najoryginalniejszy output, ale **zdyskwalifikowany**: zero MCP, chmura, efemeryczny per-notebook |

---

## 4. Werdykt: czy istnieje „bardziej oryginalna i potężna apka do budowania ogromnych kontekstów (z MCP)"?

**Uczciwa odpowiedź: NIE — nie istnieje jedno narzędzie, które jednocześnie (a) buduje wyraźnie potężniejszy/bardziej oryginalny kontekst, (b) jest MCP-server-native, (c) jest local-first + owned-markdown.** Każdy silny kandydat wygrywa 1-2 osie i przegrywa trzecią, którą Murmur trzyma.

**Konkretni kandydaci i W CZYM są potężniejsi:**
- **Pieces (LTM)** — potężniejszy w **surowym wolumenie** (cała aktywność, nie tylko spotkania, okno 9 mies.), lokalny ORAZ z MCP. Ale wygrywa osią, którą **świadomie odrzuciliśmy** — pasywny ambient-capture bez syntezy = „landfill" (potwierdza to twoja własna pamięć `brain2-multisource-rag-roadmap.md`). To nie dług, to decyzja. Brak owned-md, closed-source, brak lock.
- **Zep/Graphiti** — potężniejsze w **wymiarze czasu**: bitemporalny graf faktów z inwalidacją, odpowiada „co się zmieniło". Murmur trzyma tylko *nazwy* encji + log wzmianek (`entities`/`entity_mentions`), bez rozwiązanych, wersjonowanych faktów. To **najbardziej oryginalna brakująca zdolność**.
- **mem0** — potężniejsze w **warstwie faktów**: ekstrakcja + reconcile ADD/UPDATE/NOOP. Ale jego architektura retrievalu (multi-signal fusion) to **dokładnie to, co Murmur już ma** — brakuje tylko reconcile-warstwy na wierzchu.
- **Basic Memory** — bardziej oryginalny w **dwukierunkowości**: AI dopisuje observations/relations z powrotem do grafu przez MCP write-tools. Murmur ma MCP **read-only**. To najtańszy do przejęcia wzorzec.
- **NotebookLM** — największe okno/najoryginalniejszy output, ale **zdyskwalifikowany** (zero MCP, chmura, nie persystentny mózg).

**W czym Murmur jest UNIKALNY (kombinacja, której memory-engines NIE mają):** diaryzowany **głos jako first-class źródło** + **on-device transkrypcja** (whisper large-v3) + **entity-graph dossiers** + **owned-markdown w Obsidian** + **lokalny zero-egress MCP** + **per-folder seal/redaction firewall**, wszystko nad jednym SQLCipher. **Żaden** konkurent nie łączy tej piątki. Murmur nie jest „w tyle" — jest w 3-elementowej lidze (Pieces, Basic Memory, Murmur) MCP-server-native + local, i jako jedyny w niej ma capture pipeline + lock model.

**Bottom line:** przewaga Murmura to nie „ma MCP" (to już table-stakes) ani „ma RAG", tylko **integracja**: MCP + RAG + posiadane pliki + lock, bez wysyłania spotkań do czyjejkolwiek chmury. Realne braki to nie „więcej wektorów" — to **warstwa faktów + czasu** (Graphiti×mem0) i **realny embedder w wydaniu**.

---

## 5. Co konkretnie przejąć — rekomendacje (uszeregowane)

**R1. Skompiluj release z `--features local-embed` + napraw guard reindexu. [S, ryzyko: niskie]**
Bez tego cały dojrzały silnik RAG (sekcja 2.1) mieli stub-wektory w wydaniu — „semantyczny" RAG nie jest semantyczny. Trzeba: (a) dodać feature do komendy release/`tauri.conf.json`; (b) zmienić guard `reindex_embeddings` by sprawdzał **typ aktywnego embeddera** (`active_embedder()`), nie obecność plików (`commands.rs:2492-2493`); (c) zwalidować jakość/Polish recall @Mac. To odblokowuje istniejący kod, nie pisze nowego. Ryzyko egress: zero (e5 jest on-device). Najwyższy ROI.

**R2. Bitemporalna tabela faktów + deterministyczny reconcile (Graphiti × mem0, w SQLCipher). [M, ryzyko: średnie — lock]**
Najbardziej oryginalny ruch i najlepiej dopasowany do reguł. Nowa **additywna** tabela `facts(subject, predicate, object, valid_at, invalid_at, recorded_at, source_meeting_id)` na istniejącym SQLCipher (BEZ Neo4j/Kuzu/LanceDB). Inwalidacja = `UPDATE invalid_at`, czyli **additive, nigdy DELETE** — idealnie pod regułę „no destructive migration". Zaczepienie: rozszerza dzisiejszy `entities`/`entity_mentions` (`db.rs:172,180`), które są tylko mention-logiem. **Pierwszy plasterek headless (zero nowego mózgu):** czysta funkcja reconcile (ADD/UPDATE/INVALIDATE/NOOP) z RED-before-GREEN testem konfliktu („deadline=czwartek" → „deadline=piątek" → stary dostaje `invalid_at`, oba w historii, dossier pokazuje piątek). **Ryzyko/lock:** fakty to derywat treści → MUSZĄ być `visibility_clause`-gated + **purge-on-lock jak `vec_chunks`** (`db.rs:876`); obowiązkowy `lock-security-reviewer`. Ekstrakcja z transkryptu MUSI iść przez lokalny mózg (zero-egress), nie przez `claude_code`.

**R3. Observations/relations do markdownu (Basic Memory). [S, ryzyko: niskie]**
Rozszerz `summarize/graph.rs` z „tylko nazwy" (`graph.rs:16,23`) o typowane **observations** (`- [decision] deadline → piątek (2026-06-20)`) i **relations** (`owns [[Atlas]]`) zapisywane na stronach encji w vaulcie. Czytelny dla człowieka, traversowalny przez Obsidian/MCP, zero nowego store'u, czysto Obsidian-native. To warstwa prezentacji dla R2 (bez R2 to wciąż append → duplikaty/staleness, więc R2+R3 idą razem).

**R4. Rozszerz MCP o gated write/memory tool (Basic Memory write-back). [M, ryzyko: średnie — lock]**
Zamień read-only MCP w **dwukierunkowy mózg** — jedyną oś, gdzie Basic Memory jest „bardziej oryginalny". Dodaj 1 gated write-tool `record_observation(entity, observation, relation)` zasilający `entity_mentions`/`facts`. Zaczepienie: istniejący seam `execute_tool` (`tools.rs:69`) + powierzchnia `mcp.rs:230-262`. **Ryzyko/lock:** write musi przejść `visibility_clause`; dowód = test, że zapis do sealed-folder fails-closed, a `get_entity_dossier` po zapisie zwraca nową relację z cytatem. **Uwaga:** to łamie dzisiejszą „zero write tools" gwarancję MCP — wymaga przemyślenia z tokenem (R8) i `lock-security-reviewer`.

**R5. Chunk-level retrieval do promptu + embeduj transkrypty. [M, ryzyko: niskie]**
Dziś vec0 wybiera tylko meetingi-kandydatów, a `pack_meetings` wstawia CAŁY markdown notatki (`vault_context.rs:111-152`) — marnuje budżet 200k znaków. Przejmij od mem0/NotebookLM chunk-level grounding: pakuj **najlepsze fragmenty**, nie całe notatki; rozszerz `chunk_note` o transkrypty (dziś `source_type` zahardkodowane `'voice'`). Wymaga R1 (realny embedder). Czysto on-device.

**R6. Domyślnie WŁĄCZ token MCP + rozważ default-on redakcję nazwisk. [S, ryzyko: niskie]**
Dwa realne default-gapy z sekcji 2.3/2.5: (a) token MCP OFF → każdy lokalny proces czyta treść odblokowanych spotkań bez auth (`mcp.rs:411`); flip na default ON. (b) `NoopNameRedactor` → imiona/nazwiska egresują do chmury w standardowym buildzie. Minimum: **głośny banner UX** „nazwiska NIE są redagowane" przy egresie, docelowo R1-style kompilacja `local-ner` w release. Czysta poprawa postury prywatności, zero nowej architektury.

**R7. Sleep-time konsolidacja (Letta). [M, ryzyko: średnie — dopiero po lokalnym mózgu]**
Idle-time on-device pass: dedup faktów (R2), inwalidacja stale, przepisanie digest/dossier — zero egress. Mocny hook produktowy („drugi mózg mądrzeje gdy nie patrzysz"). **Wymaga** lokalnego mózgu (Phase 3, `local-brain`) i musi być **additive + odwracalne** (ryzyko cichej korupcji pamięci). Buduj dopiero po R2.

**R8. Pierwszy nie-głosowy connector jako lokalny tool (parytet breadth vs Otter). [M/L, ryzyko: średnie — egress]**
Otter wyprzedza wizję brain2 w multi-source breadth. Zgodnie z twoją pamięcią (`connectors-live-tools-decision.md`): zewnętrzne źródła = **on-demand agentic tools, nie wektoryzowane**. Framework już istnieje (`connectors/mod.rs`, web+kalendarz działają). Najtańszy następny: rozszerzyć istniejący kalendarz (już `EgressClass::Local`) lub dodać jeden consent-gated External. **Ryzyko/egress:** każdy nowy External MUSI iść przez `ConnectorRegistry::search` (redakcja PRZED connectorem) + fail-closed consent; głośny i justyfikowany. Pozycjonowanie marketingowe (battlecard „local MCP vs hosted MCP") to osobny, zerowy-koszt ruch wart zrobienia od razu.

**Sekwencja:** R1 + R6 natychmiast (odblokowują istniejący kod / zamykają gapy, niskie ryzyko). R2+R3 jako rdzeń nowej oryginalności (warstwa faktów+czasu — to czego front ma, a my nie). R4 zaraz po. R5 po R1. R7/R8 po lokalnym mózgu. Każda rekomendacja dotykająca lock/egress (R2, R4, R7, R8) przechodzi obowiązkowy `lock-security-reviewer` przed merge.

---

# Załącznik A — Surowe ustalenia z analizy kodu (5 agentów)

## R1 — Warstwa wektorowa / semantyczny RAG

Mam pełny obraz. Zwracam ustalenia.

---

# R1 — Warstwa wektorowa / semantyczny RAG w Murmur

## Werdykt jednozdaniowy
Cała *infrastruktura* RAG (vec0 KNN + FTS5 + RRF + GraphRAG-lite + bramkowanie lock + komendy + MCP) jest **realnie wired i przetestowana headless** — ale jedynym embedderem w buildzie domyślnym/release jest **deterministyczny hash-bag `StubEmbedder` (NIE semantyczny)**; prawdziwy model e5 jest tylko **compile-proven za `--features local-embed`**, którego release **nie kompiluje**. Semantic search jest **domyślnie OFF** (`semantic_search_enabled=false`).

---

## 1. Seam embeddingu (`embed.rs` + `embed/candle_bert.rs`)

- **Trait `Embedder`** (`embed.rs:40`): `dim()`, `embed()` (raw), plus default `embed_passage`/`embed_query` doklejające asymetryczne prefiksy e5 `"passage: "` / `"query: "` (`embed.rs:56-66,73-76`). Index woła `embed_passage`, query woła `embed_query`.
- **`EMBED_DIM = 384`** (`embed.rs:28`) — celowo równe `hidden_size` modelu **multilingual-e5-small**, więc realny model wchodzi bez migracji schematu vec0 (`embed.rs:78-81`, hard-guard `candle_bert.rs:125-130`).
- **`StubEmbedder`** (`embed.rs:113-123`): FNV-1a hash każdego tokenu → indeks w 384-wym wektorze ze znakiem, L2-normalizacja (`embed.rs:176-186`). Kontrakt to **tylko determinizm**, nie sens semantyczny (komentarz `embed.rs:109-111`). Pusty/interpunkcyjny tekst → wektor zerowy.
- **`CandleBertEmbedder`** (`candle_bert.rs:43-215`): realny e5 przez candle-transformers BERT (Metal, fallback CPU `pick_device` `:79`), **leniwe ładowanie** safetensors+tokenizer za `Mutex<Option<Arc<Loaded>>>` (`:91`), mean-pool z maską + L2 (`:219-238`) = kontrakt e5. Nagłówek pliku jest szczery: „**COMPILE-proven only**… `cargo test --lib` NEVER runs a forward pass" (`candle_bert.rs:5-18`); jakość/Polish recall/Metal weryfikowalne **tylko @Mac z modelem na dysku** (jest `#[ignore]` smoke test `:247-288`).
- **`active_embedder()`** (`embed.rs:144-162`) — degradacja w kolejności: jeśli `feature=local-embed` **i** 3 pliki e5 obecne (`embed_model_present` `:102-107`) → `CandleBertEmbedder`; w przeciwnym razie → `StubEmbedder`. Nigdy nie panikuje, nie blokuje startu.
- **Feature `local-embed`** = `dep:candle-transformers/core/nn/tokenizers`, **OFF by default** (Cargo.toml `:138-151`). `download_embed_model` (`embed.rs:268-332`) pobiera 3 pliki z HF inbound-only z `intfloat/multilingual-e5-small`.

## 2. vec0 / FTS5 — schemat i rejestracja (`storage/db.rs`)

- **`sqlite-vec = "0.1.9"` to twarda zależność** (Cargo.toml:63, nie-opcjonalna) — czyli vec0 KNN jest dostępne w KAŻDYM buildzie, niezależnie od `local-embed`. To dlatego stub-wektory mogą być zapisywane/odpytywane nawet w release.
- **`register_vec_extension()`** (`db.rs:109-128`): `sqlite3_auto_extension(sqlite3_vec_init)` przez `Once`, wołane **przed `Connection::open`** w `open_with_key` (`db.rs:150`) — bo lista auto-extension czytana jest przy otwarciu uchwytu (footgun macOS sqlite-vec #169). Rejestracja nie czyta stron → `PRAGMA key` pozostaje pierwszym SQL.
- **Schemat** (`migrate_vector` `db.rs:343-367`): tabela plaintext `note_chunks(id, meeting_id, provider_id, chunk_idx, source_type DEFAULT 'voice', text, content_hash)` + wirtualna `vec_chunks USING vec0(chunk_id INTEGER PRIMARY KEY, embedding float[384])`. Mapowanie 1:1 `vec_chunks.chunk_id == note_chunks.id`.
- **FTS5** (`migrate_fts` `db.rs:387-455`): TRZY external-content tabele (`fts_meetings`/`fts_segments`/`fts_notes`) z triggerami `_ai/_ad/_au`. Tokenizer **`unicode61 remove_diacritics 2`** — Polish-aware (ł/ż/ó/ą...). Seal blankuje plaintext → trigger `_au` czyści stare tokeny z indeksu.

## 3. Kiedy embeddingi są liczone i zapisywane

- **Na zapis notatki**, jeden produkcyjny call-site: `summarize_and_export` (`pipeline.rs:562-574`) — **tylko gdy `config.semantic_search_enabled`** → `index_meeting_if_enabled` → `index_meeting_chunks`. Pokrywa też re-summarize (`resummarize_existing` `pipeline.rs:829` woła `summarize_and_export`). Best-effort: błąd loguje, nigdy nie wywala pipeline'u.
- **`index_meeting_chunks`** (`db.rs:807-871`): chunkuje **markdown notatki** (`chunk_note` `embed.rs:215-239`: akapity scalane do ~800 znaków, każdy z nagłówkiem `<title> · <date>`), `embed_passage`, potem **PURGE-then-insert w jednej transakcji** (`purge_chunks_tx` `:896-912`: vec0 najpierw, potem note_chunks). `content_hash` (FNV-1a) zapisany pod ewentualny inkrementalny re-index, **ale dedup po hashu nie jest jeszcze użyty** — zawsze clean replace.
- **Bramka indeksu** `index_meeting_if_enabled` (`pipeline.rs:696-710`): `enabled==false` → no-op; `meeting_is_visible==false` (sealed-not-unlocked) → no-op. Nigdy nie chunkuje plaintextu zalockowanego folderu.
- **Backfill** `reindex_embeddings` (`commands.rs:2477-2563`, zarejestrowana w `lib.rs:140`): korpus = `list_meetings_visible(unlocked)`, każda przez `get_note_if_visible` → `index_meeting_chunks`. Zwraca `{status, indexed, total}`.

## 4. Retrieval — KNN + FTS fusion (to JEST RRF/hybryda, i to GraphRAG)

- **`search_semantic_visible`** (`db.rs:937-995`): KNN izolowany w CTE (`WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance`), wizibilność `visibility_clause` (`db.rs:3056-3068`) dołożona POZA CTE; dedup do jednego (najbliższego) chunku na meeting.
- **`search_hybrid_visible`** (`db.rs:1007-1062`): **RRF (k=60, `rrf_fuse` `embed.rs:245-259`) nad TRZEMA listami** — FTS5/BM25 (`search_visible`), wektor (`search_semantic_visible`), oraz **entity-graph neighbourhood** (`entities_matching_query` → `meetings_mentioning_entities_visible`). To leg grafowy = **GraphRAG-lite**, którego flat-RAG nie ma: zapytanie nazywające znaną encję ("Project Atlas") wciąga jej cross-meeting sąsiedztwo. Brak encji → pusta lista → fuzja byte-identyczna z FTS∪wektor.
- **Powierzchnie używające hybrydy** (obie bramkowane flagą):
  1. **Ask-My-Vault** (`commands.rs:1390-1412`): flaga ON → `build_vault_context_hybrid_visible` (`vault_context.rs:87-104`); flaga OFF → czysty FTS `build_vault_context_visible`. Budżet korpusu **200 000 znaków ≈ 50k tok** dla Claude, 4k dla Ollama (`vault_context.rs:21-31`).
  2. **MCP `search_semantic`** (`tools.rs:84-108`, `mcp.rs:248-251,315-345`): flaga OFF → zwraca literalne "Semantic search is disabled" (NIE cichy fallback); ON → `search_hybrid_visible`.
- **KLUCZOWE ROZRÓŻNIENIE — always-on grounding nie używa wektorów.** `related_context.rs` (Phase 4, grounding nowej notatki w poprzednich) jest **FTS-only**: `salient_query` → `search_visible` + `get_note_if_visible` (`related_context.rs:5-8,147-197`). „Uses the LIVE FTS5 retrieval… NO local/embedding model" (`related_context.rs:5`). Czyli zawsze-działająca część RAG to leksykalny FTS, nie semantyka.

## 5. Bramkowanie lock/visibility (spójne)

- Każdy odczyt wektorowy przechodzi przez ten sam `visibility_clause` co FTS (sealed-not-unlocked → niewidoczne), defense-in-depth nawet gdyby chunk uciekł purge.
- **Wektory są odwracalne**, więc chunki/wektory istnieją tylko dla widocznej treści: `purge_chunks_for_meetings` w tej samej transakcji co blankowanie plaintextu przy seal/relock (`db.rs:876-912`, `lock_folder`), oraz osobny purge w `delete_meeting` (vec0 nie łapie się na CASCADE — test `vec_chunks_purged_on_delete_meeting` `db.rs:4252`). Testy: `vec_semantic_search_is_gated_by_visibility` (`:4174`), `vec_chunks_purged_on_lock` (`:4207`).
- MCP czyta flagę z tej samej zaszyfrowanej tabeli `settings`, fail → default OFF (`mcp.rs:341-345`).

## 6. Stan dojrzałości — wired / gated / stub

| Element | Stan |
|---|---|
| vec0 KNN + FTS5 + RRF + GraphRAG-lite + gating + purge | **WIRED**, w każdym buildzie, unit-tested headless |
| komendy `embed_model_present`/`download_embed_model`/`reindex_embeddings` + flaga settings | **WIRED** (`lib.rs:138-140`) |
| Ask-My-Vault hybryda + MCP `search_semantic` | **WIRED**, ale za flagą |
| `semantic_search_enabled` | **domyślnie OFF** (`config.rs:223`) |
| Realny embedder e5 `CandleBertEmbedder` | **GATED** za `--features local-embed` (OFF by default, **NIE w release build**); tylko compile-proven |
| Embedder w buildzie domyślnym/release | **STUB** (`StubEmbedder`, hash-bag, NIE semantyczny) |
| Grounding nowych notatek (always-on) | FTS-only, **bez wektorów** |

## 7. Luki vs prawdziwy „huge context"

1. **Release nie kompiluje `local-embed`.** Komenda release to `npx tauri build --target universal-apple-darwin --bundles app` (`release-murmur/SKILL.md:141`), bez `--features` (potwierdzone: brak feature w `tauri.conf.json`, `scripts/`, `.cargo/config.toml`). Czyli w wydanym buildzie `candle_bert` jest wycięty → nawet po pobraniu modelu i włączeniu flagi „semantic" to nadal **stub-wektory** (zero realnego sensu / multilingual / Polish recall).
2. **Footgun bramki reindex.** `reindex_embeddings` przekazuje `model_present = embed_model_present()` (sama obecność 3 plików) ORAZ `active_embedder()` (`commands.rs:2492-2493`). W buildzie bez feature `embed_model_present()` może być `true`, a `active_embedder()` to wciąż `StubEmbedder` → `reindex_embeddings_inner` przechodzi guard i **zaindeksuje stub-wektory**, mimo że doc obiecuje „no stub indexing" (`commands.rs:2469-2472,2522-2529`). Guard sprawdza pliki, nie typ aktywnego embeddera.
3. **Granularność prompt = cała notatka, nie chunk.** vec0/chunk KNN wybiera tylko **meetingi-kandydatów** (top 40), a `pack_meetings` wstawia do promptu **CAŁY markdown notatki** truncowany budżetem (`vault_context.rs:111-152`), nie wyszukane chunki. Brak chunk-level retrieval do promptu, brak rerankera, brak okna kontekstu zbudowanego z najlepszych fragmentów.
4. **Tylko notatka jest embedowana, nie transcript.** `chunk_note` chunkuje markdown notatki; `source_type` zahardcodowane `'voice'`; brak segment-level/transkrypcji w wektorach.
5. **Jakość = nieweryfikowalna headless.** e5 forward pass, mean-pool, prefiksy e5, Polish recall, Metal perf — wszystko „@Mac, signed build" wg autora (`candle_bert.rs:5-18`); zielony `cargo test --lib` niczego z tego nie dowodzi.
6. **Brak inkrementalnego re-indexu** (mimo `content_hash`) — każdy zapis to pełny purge+reinsert notatki; brak triggera reindeksu na edycję poza (re)summarize/manualnym backfillem.

**Wnioski:** „silnik" RAG jest dojrzały i poprawnie bramkowany pod model lock; brakuje *mózgu* — realnego embeddera w wydaniu. Żeby dostać prawdziwy semantyczny „huge context" trzeba: (a) kompilować release z `--features local-embed`, (b) naprawić guard reindexu by sprawdzał faktyczny typ embeddera (a nie obecność plików), (c) rozważyć chunk-level retrieval do promptu zamiast pakowania całych notatek, (d) zembedować również transkrypcje, (e) zwalidować jakość/Polish @Mac.

Pliki kluczowe: `src-tauri/src/embed.rs`, `src-tauri/src/embed/candle_bert.rs`, `src-tauri/src/storage/db.rs` (vec0/FTS/hybryda), `src-tauri/src/pipeline.rs:562`, `src-tauri/src/summarize/{related_context.rs,vault_context.rs}`, `src-tauri/src/tools.rs:84`, `src-tauri/src/mcp.rs:315`, `src-tauri/src/commands.rs:{1390,2477}`, `src-tauri/src/settings/config.rs:137`.

## R2 — Modele AI: transkrypcja i fazy rozmowy

Mam komplet. Poniżej gęste ustalenia.

---

# R2 — Modele AI w transkrypcji i „fazach rozmowy" (Murmur, on-device)

Trust-code uwaga wstępna: `default = []` w `src-tauri/Cargo.toml:140` — **wszystkie trzy ciężkie modele (`local-brain`, `local-embed`, `local-ner`) są WYŁĄCZONE w domyślnym buildzie**. `whisper-rs` (Cargo.toml:31) i `sherpa-onnx` (Cargo.toml:35) to deps NIE-opcjonalne → zawsze skompilowane.

## A. Modele transkrypcji (zawsze wired, on-device)

### 1. whisper.cpp (whisper-rs 0.16, Metal) — RDZEŃ, w pełni wired
- Backend Metal wybierany compile-time przez feature `metal`; GPU auto na Apple Silicon (`whisper.rs:74-106`). `WhisperContext` ładowany raz, reużywany (`whisper.rs:68-106`).
- Domyślny model **large-v3** multilingual (`model.rs:43`, `AppConfig::default().model_size = "large-v3"` config.rs:213). Wybór modelu: `model_filename(size, language)` (`model.rs:41-52`) — buildy `.en` tylko dla tiny/base/small/medium przy `language=="en"`; large-v3/turbo są multilingual-only, więc **polski zawsze trafia w pełny `ggml-large-v3.bin`** (`model.rs:46-51`, testy 237-243). Pobierany z mirrora ggerganov/whisper.cpp HF (`model.rs:56-58`), download atomowy (`model.rs:177-212`). Język: `config.language` (domyślnie `None` = auto-detekcja, config.rs:200), forsowanie `pl` biasuje priory dekodera (`whisper.rs:142-144`).
- **Dwa profile dekodowania w JEDNEJ implementacji** (`TranscribeQuality`, `whisper.rs:28-34`, `build_params` 216-251):
  - **Fast** — greedy `best_of:1`, bez fallbacku (`whisper.rs:223`). Używany w LIVE captions i wake/voice-trigger.
  - **Accurate** — beam search width 5, drabina temperatur 0.0→1.0 co 0.2, bramki anti-halucynacyjne (entropy 2.4 / logprob -1.0 / no_speech 0.6), `condition_on_previous_text` ON (`whisper.rs:42-61`, 231-249). Używany w batchu post-Stop, kluczowy dla fleksyjnego polskiego.

### 2. Silero VAD (whisper.cpp natywny `WhisperVadContext`, **CPU**) — wired, gated configiem
- Model `ggml-silero-v5.1.2.bin` (~885 kB) z repo ggml-org/whisper-vad (`model.rs:21,62-64`). Ładowany **na CPU celowo** — drugi kontekst ggml-Metal robił `ggml_abort` (twardy C-abort) obok głównego whisper-Metal (`vad.rs:36-50`).
- Tylko ścieżka batch (post-Stop). Pre-segmentuje strumień na REGIONY mowy → każdy region to osobny `transcribe_with` ze świeżym stanem (reset kontekstu przez długie ciszy), spany sklejane przy gap < 2 s (`vad.rs:22-99`, `pipeline.rs:131-162`). Bramka: `config.vad_enabled` (**domyślnie true**, config.rs:209). Brak modelu → degrade do całego bufora (`pipeline.rs:312-322`). Uwaga: `FullParams::enable_vad` to potwierdzony no-op/panic — stąd standalone wrapper (`vad.rs:4-6`).

### 3. Diaryzacja sherpa-onnx — wired, ale **opt-in (domyślnie OFF)**
- Dwa modele ONNX (sherpa-converted), pobierane on-demand: **pyannote segmentation 3.0** (`sherpa-pyannote-segmentation-3.0.onnx`, ~12 MB) + **WeSpeaker CAM++ embedding** (`wespeaker_en_voxceleb_CAM++.onnx`, ~28 MB) (`model.rs:25-26,154-173`). Pipeline: segmentacja → embeddingi → fast clustering z auto-liczbą mówców (próg 0.5, `num_clusters=-1`) (`diarize.rs:1-55`).
- **Tylko strumień „others" (system audio)**; mic „me" nigdy nie diaryzowany (`diarize.rs:1-2`, `pipeline.rs:386-397`). Relabel: segment dostaje `others-{n}` po max-overlap; przy ≤1 mówcy zostaje plain `others` (`diarize.rs:84-106`). Bramka: `config.diarize_others` (**domyślnie FALSE**, config.rs:211). Best-effort: każdy fail → pojedynczy label „others" (`diarize.rs:5-6`). Statyczny onnxruntime (macOS 13.4+).

Uwaga rozróżniająca: **„me/others" to NIE diaryzacja per-osoba** — przypisanie wynika z tego, KTÓRY strumień capture wyprodukował segment (merge po wall-clock w `audio/merge`), patrz honest-dent w `types.rs:11-15`. Diaryzacja sherpa dokłada N-way dopiero NA „others".

## B. Lokalny „brain" (reason.rs + reason/mistral.rs)

### 4. MistralReasoner (mistralrs 0.8.1 GGUF, Metal, in-process) — **feature-gated `local-brain`, OFF default, COMPILE-proven only**
- Ładowany TYLKO gdy: feature `local-brain` ON **i** GGUF obecny na dysku **i** `brain_backend == Local` (`reason.rs:278-305`). Domyślny `brain_backend` = **Cloud** (config.rs:226 + BrainBackend::default → `from_str_or_default` zwraca Cloud, config.rs:47, test 601). Lazy load (model ładuje się przy 1. wywołaniu, nie blokuje startu, `mistral.rs:56-105`).
- Rejestr `BRAIN_MODELS` (`reason.rs:55-86`): **Bielik 11B v3 Q4_K_M** (arch llama, pl/en, ~6.7 GB, min 10 GB RAM — pierwszy/polski-native), **Qwen3 14B** (qwen3, ~9 GB), **Qwen2.5 3B** (qwen2, ~2 GB, low-RAM). Tylko archy parse-safe dla mistralrs: llama/qwen2/qwen3 (guard-test `reason.rs:662-671`).
- **Structured JSON NIE przez constrained decode** — `Constraint::JsonSchema` przepełniał kontekst na Bieliku (błąd „narrow start:32768" na granicy 32K), więc instrukcja schematu w promcie + `parse_first_json` (ten sam pattern co CloudReasoner) (`mistral.rs:132-145`). Honest scope: tylko link/typecheck w CI; jakość inferencji, polski, Metal-perf weryfikowalne wyłącznie na podpisanym Macu z modelem (`mistral.rs:5-16`).

### 5. CloudReasoner — **DOMYŚLNY brain, NIE lokalny model**
- `active_reasoner` przy `brain_backend=Cloud` (default) zwraca CloudReasoner (`reason.rs:258-272`), który implementuje `LocalReasoner` deleguje do chmurowego LLM przez **ten sam `make_provider`** co podsumowanie → dziedziczy fail-closed `cloud_egress_consented` gate + RedactingProvider (`reason.rs:411-531`). Bez zgody (default `cloud_egress_consented=false`, config.rs:222) → `Err` → deterministyczna podłoga.

### 6. StubReasoner — deterministyczna podłoga (`reason.rs:380-409`)
- Aktywny gdy `brain_backend=Off`, lub `Local` bez modelu, lub feature off (`reason.rs:266-305`). **NIE jest domyślny** — default to CloudReasoner. `id()=="stub"` przełącza orchestrate na czysto deterministyczną ścieżkę.

## C. NER DeBERTa (summarize/ner_deberta.rs) — **feature-gated `local-ner`, OFF default**

### 7. DebertaV2 NER (candle-transformers 0.10.2, Metal/CPU) — **wired tylko jako firewall-redaktor, NIE do grafu encji**
- Rola: w `RedactingProvider` maskuje **nazwiska osób** (`B-PER`/`I-PER` → `⟪NAME_n⟫`) PRZED egresem do chmury; odwracane w odpowiedzi (`ner_deberta.rs:1-20`). To zamyka lukę regexowego firewalla, który nazwisk nie czyścił. **Invariant: tylko USUWA/maskuje, nigdy nie dodaje** → miss leakuje nie więcej niż dzisiejszy Noop (`ner_deberta.rs:14-20`).
- Wpięcie: `make_provider` → `RedactingProvider::with_name_redactor(..., active_name_redactor())` (`summarize/mod.rs:125-127`, redact.rs:78-95). Domyślnie (feature off / brak modelu) → **NoopNameRedactor** (nazwiska przechodzą, egress byte-identyczny, redact.rs:227-230, 290-294). Model-agnostyczny (PERSON po sufiksie labela), target mDeBERTa-v3 multilingual safetensors+tokenizer+config (`ner_deberta.rs:22-31`). Compile-proven; tylko BIO→span decode unit-testowany headless (`ner_deberta.rs:32-39, 382-507`).
- **Graf encji to OSOBNA, CHMUROWA ścieżka** — `build_and_persist_entities` → `graph::extract_entities(provider, …)` → `provider.complete(SYSTEM, user)` (`commands.rs:1251-1266`, graph.rs:31). Timeline analogicznie cloud (timeline.rs:48). DeBERTa NER tu nie uczestniczy.

### (8. e5-small embedder — poza ścisłym R2, ale ten sam wzorzec)
- `local-embed` OFF default → **StubEmbedder** (hashowany bag-of-tokens, NIE semantyczny, embed.rs:109-127). Realny `CandleBertEmbedder` (multilingual-e5-small 384-dim) tylko przy feature ON + model present (embed.rs:144-161). Używany do RAG-indeksu/zapytań, bramkowany `semantic_search_enabled` (**domyślnie false**, config.rs:223).

## D. Fazy rozmowy i pipeline

**Brak nazwanego „phase" enuma** — fazy są strukturalne. Mapowanie:

- **FAZA LIVE (w trakcie nagrania)** — pętla `transcribe/live.rs` (osobny OS-thread, tick 3 s, okno 14 s, read-only snapshot bufora):
  - Captions: whisper **Fast** (`live.rs:154-158`).
  - Wake-word „Klaudku…": `detect_wake`/`parse_voice_intent` (deterministyczny parser PL/EN), dedup 5 ticków (`live.rs:33,712-727`). **Surface-only** chyba że `realtime_reactions` ON (**domyślnie false**, config.rs:227) — wtedy dispatch akcji (`live.rs:237-257,270-272`).
  - Manualny voice-command (click-to-stop): whisper Fast na oknie post-click, język forsowany z `config.language` (`live.rs:462-562`).
  - Dispatch akcji (Flow B): `resolve_command_intent` = keyword fast-path → **brain `reasoner.structured`** (`interpret_with_brain`, voice_action.rs:176-197) → fallback Research; potem `handle_voice_action` (RAG, gated, consent-gated brain) off-thread (`live.rs:286-334,378-428`).

- **FAZA POST-MEETING (po Stop)** — `pipeline.rs::run_after_stop` → `run_inner`:
  1. Dual-stream 16 kHz (mic + opcjonalny system audio z sidecara ScreenCaptureKit), AEC mic do ASR, archiwum = mix (NIE do whispera) (`pipeline.rs:190-257`).
  2. Transkrypcja **Accurate** OBU strumieni, VAD-segmentowana; diaryzacja sherpa na „others"; merge po wall-clock → me/others (`pipeline.rs:346-414`).
  3. Pełny tekst BEZ wypowiedzi do asystenta („Klaudku…", `is_assistant_directed`, `pipeline.rs:425-435`).
  4. Summarize chmurowym providerem (default `provider_id="claude_code"`, config.rs:196) przez `make_provider` (firewall + opcjonalny NER) (`pipeline.rs:527-541`).
  5. Graf encji (cloud), indeks semantyczny (gated embed) — best-effort (`pipeline.rs:556-574,657-661`).

- **FAZA ENRICH (grounding/RAG w trakcie summarize)** — `orchestrate.rs::orchestrate_context` (Flow A): jeśli reasoner=stub → **deterministyczna podłoga** (`build_grounding_context`, byte-identyczna); inaczej **brain planuje retrieval** (`reasoner.structured` → plan → gated `execute_tool` → cited corpus) (`orchestrate.rs:112-160`). Korpus EGRESuje w promcie, więc każdy read gated przez `visibility_clause`/`search_visible`. W default+no-consent CloudReasoner.structured zwraca Err → i tak podłoga.

## E. Mapa „model → faza → on-device/chmura → wired/gated"

| Model | Faza | Lokacja | Wired/Gated (file:line) |
|---|---|---|---|
| whisper.cpp large-v3, **Fast** | LIVE captions/wake/manual | on-device Metal | WIRED, zawsze (`whisper.rs:223`, `live.rs:154-158`) |
| whisper.cpp large-v3, **Accurate** | POST-MEETING batch | on-device Metal | WIRED, zawsze (`whisper.rs:231-249`, `pipeline.rs:152`) |
| Silero VAD (ggml) | POST-MEETING (pre-seg) | on-device **CPU** | WIRED, gated `vad_enabled` **=true** (`vad.rs:41-50`, config.rs:209) |
| pyannote-seg 3.0 + CAM++ (sherpa ONNX) | POST-MEETING diaryzacja „others" | on-device (static ORT) | WIRED, gated `diarize_others` **=false** (`diarize.rs:35-55`, config.rs:211) |
| MistralReasoner (GGUF: Bielik/Qwen) | ENRICH (Flow A) + LIVE (Flow B) | on-device Metal in-proc | **GATED `local-brain` OFF** + GGUF + `brain_backend=Local`; default Cloud → **nieaktywny**; compile-only (`reason.rs:278-305`, `mistral.rs:5-16`) |
| CloudReasoner | ENRICH + LIVE dispatch | **CHMURA** (make_provider) | WIRED, **default brain**; consent-gated (`reason.rs:258-272,411-531`) |
| StubReasoner | podłoga | on-device (deterministyczny) | WIRED gdy Off/Local-bez-modelu; **nie default** (`reason.rs:380-409`) |
| DeBERTa-v2 NER (candle) | POST-MEETING firewall (mask nazwisk przed egresem) | on-device Metal/CPU | **GATED `local-ner` OFF**; default **Noop**; compile-only (`ner_deberta.rs`, redact.rs:78-95) |
| Graf encji / timeline | POST-MEETING | **CHMURA** `provider.complete` | WIRED (graph.rs:31, timeline.rs:48) — **NIE lokalny NER** |
| e5-small embedder (candle) | ENRICH RAG index/query | on-device Metal/CPU | **GATED `local-embed` OFF** + `semantic_search_enabled=false`; default **StubEmbedder** (embed.rs:144-161, config.rs:223) |

## Wnioski (bez ślepego certyfikowania)
1. **Realnie działające on-device w domyślnym buildzie:** whisper.cpp (Fast+Accurate) i Silero VAD. Diaryzacja sherpa jest wpięta i działa, ale **wyłączona domyślnie** (opt-in) i tylko na „others".
2. **Lokalny brain, lokalny embedder i lokalny NER to szkielety za feature-flagami WYŁĄCZONYMI domyślnie** (`local-brain`/`local-embed`/`local-ner`) — w produkcyjnym buildzie aktywne są ich dependency-free zamienniki: **CloudReasoner** (default brain, chmura+consent), **StubEmbedder** (niesemantyczny), **NoopNameRedactor** (nazwiska nieczyszczone). Realny model brain/NER/embed jest **compile-proven only** — jakość/Metal/polski weryfikowalne tylko na podpisanym Macu z modelem na dysku.
3. **Pojęciowa pułapka do zaznaczenia:** graf encji NIE używa lokalnego DeBERTa-NER — encje wyciąga **chmurowy** provider; DeBERTa służy wyłącznie do maskowania PERSON w firewallu egresu. „me/others" to atrybucja strumienia, nie diaryzacja głosu (diaryzacja dokłada N-way osobno).

Pliki kluczowe (absolutne): `/Users/jakubgawronski/Projects/meetnotes/src-tauri/src/transcribe/{whisper.rs,model.rs,vad.rs,diarize.rs,live.rs,types.rs}`, `/Users/jakubgawronski/Projects/meetnotes/src-tauri/src/{reason.rs,pipeline.rs,orchestrate.rs,voice_action.rs,embed.rs}`, `/Users/jakubgawronski/Projects/meetnotes/src-tauri/src/reason/mistral.rs`, `/Users/jakubgawronski/Projects/meetnotes/src-tauri/src/summarize/{ner_deberta.rs,redact.rs,graph.rs,timeline.rs,mod.rs}`, `/Users/jakubgawronski/Projects/meetnotes/src-tauri/src/settings/config.rs`, `/Users/jakubgawronski/Projects/meetnotes/src-tauri/Cargo.toml`.

## R3 — Providerzy LLM / firewall redakcji

Mam pełny obraz. Zwięzłe, gęste ustalenia poniżej.

---

# R3 — Warstwa podsumowań / providerzy LLM / firewall redakcji

Wszystkie ścieżki potwierdzone w kodzie. Katalog `src-tauri/src/summarize/` (21 plików). Trust-code: kod jest DUŻO dalej niż CLAUDE.md (v0.3.0) — to brain2 Phase 4/5b z RAG, dossier, NER-redakcją i orchestracją.

## 1. Seam providerów (swappable, jeden trait)

**Trait** `SummarizerProvider` (`provider.rs:42`): cztery metody — `id()`, `availability()` (tania sonda, nie failuje), `summarize(req)` → gotowy Markdown Obsidian, `complete(system,user)` → surowy tekst (timeline/graph/chat/dossier itd.). Async, `Send+Sync`, async-trait.

**Trzy implementacje, jeden konstruktor** `make_provider(id, config)` (`mod.rs:64`):
- `DEFAULT_PROVIDER_ID = "claude_code"` (`mod.rs:37`); domyślny `config.provider_id = "claude_code"` (`settings/config.rs:196`).
- Klasyfikacja chmury `is_cloud()` (`mod.rs:54`): **claude_code I anthropic = chmura**, ollama = lokalny. Kluczowy fakt (`mod.rs:48-56`): claude_code to cienki klient — CLI `claude` uploaduje transkrypt do chmury Anthropic, więc traktowany jak `anthropic`.
- **Bramka zgody fail-closed (E10)** (`mod.rs:70`): jeśli `is_cloud(id) && !config.cloud_egress_consented` → `AppError::Unavailable` — żaden cloud provider nie powstaje, więc nic nie może wyjść zanim user raz nie zatwierdzi. `cloud_egress_consented` default `false` (`config.rs:222`), PRESERVE-ONLY. ollama nigdy nie bramkowany (`mod.rs:101-106`, return early UNwrapped).
- **Firewall redakcji owija OBA cloud providery** (`mod.rs:124-129`): `RedactingProvider::with_name_redactor(inner, active_name_redactor())`. ollama omija (zwrócony wcześniej, bez wrappera).

**Każdy „mózg" buduje providera tym samym seamem** — `make_provider(&config.provider_id, &config)` jest wołany w `commands.rs` przy: chat (`742`), recipes (`985`), graph extract (`1264`), ask_vault (`1420`), dossier (`1456`), digest (`1517`), brief (`1672`), timeline (`2130`), oraz w `pipeline.rs:540` (główny summarize). Wszystkie dziedziczą firewall + bramkę zgody. `all_providers(config)` (`mod.rs:137`) — fan-out availability w Settings (best-effort, brak klucza Keychain → keyless Anthropic = `Unavailable`).

### Co każdy provider używa / gdzie klucz / jaki model

**claude_code** (`claude_code.rs`, domyślny) — shell-out `claude -p` (tokio Command):
- Model: domyślnie **CLI sam wybiera** (`model=""` → brak flagi). Override `--model <id>` tylko gdy ustawiony `config.provider_model` (`mod.rs:83`, `model_args()` `claude_code.rs:410`). Effort N/A (CLI nie ma flagi).
- Hermetyzacja: `env_clear()` + tylko `PASSTHROUGH_ENV` (HOME/USER/LANG/…; `claude_code.rs:20,24`) → **sekrety/MURMUR_DEV_* NIGDY nie wyciekają do dziecka** (F2). `--disallowedTools` = wszystkie narzędzia (Bash/Edit/Write/Read/Web…; `claude_code.rs:40`) → run szczelny.
- Binarka wetowana (F3): `resolve_binary` (`:104`) + `vet_binary` (`:123`) — musi być regularnym plikiem, własność bieżącego uid, nie world-writable; `getuid` przez lokalny extern bez crate libc (`:160`).
- Timeout 180 s + `kill_on_drop` (`:15,:299`). Walidacja: output MUSI zaczynać się od `---` front-matter (`starts_with_frontmatter` `:420`). stderr NIGDY logowany na poziomie z PII (`:314`).

**anthropic** (`anthropic.rs`) — bezpośrednie HTTP `reqwest`:
- **Klucz z Keychain**: `get_secret(ANTHROPIC_KEY_ACCOUNT)` gdzie account = `"anthropic_api_key"` (`mod.rs:46,87`); provider NIGDY sam nie czyta Keychain (przekazany w konstruktorze). Service Keychain = `com.meetnotes.app`.
- Model: precedencja `provider_model` > `anthropic_model` (`mod.rs:90`); default `DEFAULT_MODEL = "claude-opus-4-8"` (`anthropic.rs:10`; też `config.rs:201`).
- Endpoint `https://api.anthropic.com/v1/messages` (`:8`), `anthropic-version: 2023-06-01` (`:9`), `max_tokens=16000` (`:13`). Klient: **TLS floor 1.2** (odmawia 1.0/1.1) + timeout 120 s/connect 15 s (E12, `:81-88`).
- Effort → **adaptive thinking**: `apply_effort` (`:60`) wstrzykuje `{"thinking":{"type":"adaptive"},"output_config":{"effort":<low|medium|high>}}`; `""`/`"default"`/nieznany tier → body NIETKNIĘTE (fail-safe, bo stary `thinking.enabled`/`budget_tokens` daje 400 na Opus 4.8). `stop_reason=="refusal"` → błąd (`:175`).

**ollama** (`ollama.rs`) — lokalny HTTP, BRAK egresu:
- `http://localhost:11434` + model `llama3.1` default (`:10-11`; `config.rs:204-205`). `/api/generate` `stream:false`. availability = `GET /api/tags` z 500 ms timeout (`:60`). Lokalny → bez firewalla, bez bramki zgody.

## 2. Firewall redakcji (`redact.rs`) — co przekracza granicę chmury i jak chronione

**Zawsze-on warstwa regex** (`redact.rs:171-182`): emaile, „karty" (ciągi 13-19 cyfr), telefony. Kolejność istotna — karty PRZED telefonami (`:251-253`), żeby długi ciąg cyfr stał się CARD nie PHONE. Zamiana na stabilne tokeny `⟪EMAIL_1⟫`/`⟪CARD_n⟫`/`⟪PHONE_n⟫` (`:269`); ten sam string → ten sam token (mapa `rev`). `restore()` odtwarza oryginały w ODPOWIEDZI (`:193`).

**`RedactingProvider`** (decorator, `:283`) — obie metody:
- `summarize` (`:319`): WSPÓLNA mapa redakcji dla `transcript` ORAZ `related_context` (Phase-4 RAG corpus EGRESUJE w prompcie — scrubowany tym samym firewallem; `:326-332`). Po regex — warstwa nazw. Restore obu warstw w replyu (`:345-346`).
- `complete` (`:349`): wspólna mapa dla `system` + `user` (oba prompty), restore w replyu.

**Co PRZEKRACZA granicę chmury (claude_code/anthropic)**, po redakcji:
- `transcript` (pełny), `related_context` (RAG — korpus wcześniejszych notatek, gated-visible), `template`/system-prompt (instrukcje + language_directive), `vault_titles` (lista tytułów notatek jako cele `[[link]]`), `meta` (data/duration/język). Dla `complete()`: system+user każdego mózgu (chat, recipes, timeline, graph, dossier, digest, brief, vault_chat, organize).
- Chronione = emaile/karty/telefony zamienione na tokeny. **NIE redagowane domyślnie: NAZWISKA/IMIONA.**

**Warstwa nazw — SEAM, domyślnie WYŁĄCZONY (uczciwie)**:
- Trait `NameRedactor` (`:216`). Domyślnie `NoopNameRedactor` (`:227`) → tekst **byte-identyczny**, pusta mapa → **imiona wychodzą do chmury bez zmian** (potwierdzone testami `:457,:466`).
- Realny redaktor `DebertaNameRedactor` (mDeBERTa-v3 PERSON-NER) jest w `ner_deberta.rs`, **kompilowany TYLKO pod `#[cfg(feature="local-ner")]`** (`mod.rs:20-21`). `active_name_redactor()` (`:78`) zwraca go TYLKO gdy `local-ner` ON **i** model obecny na dysku (`ner_model_present()` `:61`); inaczej (default build, brak modelu, błąd init) → Noop, graceful, nigdy nie panikuje/blokuje. Model: `Davlan/mdeberta-v3-base-ner-hrl` (`:48`), pobierany INBOUND-ONLY przez `download_ner_model` (`:105`, bez wysyłania treści). **Wniosek: w buildzie domyślnym redakcja nazwisk to NO-OP — imiona/nazwiska egresują do Anthropic/claude.** Recall PL = niezweryfikowany (eval @Mac, komentarz `:46`).

**Co NIE wychodzi nigdy**: cokolwiek przez ollama (lokalny). Dla chmury: high-confidence PII regex (email/card/phone) — zawsze stokenizowane. Sealed-and-not-unlocked treść — odcięta UPSTREAM bramkami widoczności (patrz niżej), nie przez redact.rs.

## 3. Lista „mózgów" (file:line, rola, zasilana powierzchnia)

| Mózg | Plik | Co robi | Powierzchnia | LLM? |
|---|---|---|---|---|
| **template** | `template.rs:10` (`default_template`), `:63` (`template_for_style` brief/detailed/action), `:122` (`language_directive`), `:150` (`build_template`), `:175` (`render_user_content` — wstrzykuje `related_context`) | Kanoniczny prompt note-format (front-matter `---` + sekcje), warianty stylu, dyrektywa języka | **note** (pipeline) | system-prompt |
| **related_context** | `:86` (`salient_query`), `:147` (`build_related_context`) | Phase-4 RAG, **ZAWSZE ON**, FTS5 (BEZ embeddingów), top-4 wcześniejszych notatek jako grounding; gated `search_visible`+`get_note_if_visible`; wyklucza self | **note** (grounding, `pipeline.rs:735`) | zasila prompt |
| **timeline** | `:32` (`generate`) | Speaker-turns + topic-spans z transkryptu (whisper nie diaryzuje) → **strict JSON** via `reason::parse_first_json` | **detail** (timeline UI), `commands.rs:2130` | `complete()` |
| **graph** | `:24` (`extract_entities`) | Ekstrakcja people/projects → strict JSON → strony `[[Person]]`/`[[Project]]` | **graph** (encje), `commands.rs:1264` | `complete()` |
| **action_items** | `:14` (`parse_action_items`), `:40` (`patch_tasks_markdown`) | Parsuje `- [ ]` linie, dokleja `📅 due`; **PURE, BEZ LLM**; dropuje komendy do asystenta (`wake::is_assistant_directed` `:20`) | **detail** (zadania), `commands.rs:1009,1048` | nie |
| **threads** | `:19` (`build_threads`) | Klastrowanie topic-spanów cross-meeting w wątki; **DETERMINISTYCZNE, BEZ LLM** | **graph/threads** UI, `commands.rs:1563` | nie |
| **chat** | `:11` (`build`) | Grounded Q&A nad JEDNYM transkryptem (system=transkrypt+historia) | **detail** ("Chat with meeting"), `commands.rs:743` | `complete()` |
| **vault_chat** | `:8` (`build`) | "Ask-My-Vault" prompt: Q&A cross-meeting z inline cytatami `[[Title]]` | **Ask**, `commands.rs:1421` | `complete()` |
| **vault_context** | `:54` (`build_vault_context_visible`), `:87` (`..hybrid_visible`) | Buduje korpus past-meetings dla Ask; gated (`search_visible`/`list_meetings_visible`/`get_note_if_visible`); hybrid=FTS∪vector RRF | **Ask** + **brief**, `commands.rs:1398,1406,1666` | zasila prompt |
| **recipes** | `:10` (`BUILTIN_RECIPES`), `:59` (`build_recipe_prompt`) | Builtin/saved prompty nad 1 transkryptem (follow-up email, decision-log, ticket, 1:1, standup, sales, interview) | **detail** (Recipes), `commands.rs:986` | `complete()` |
| **organize** | `:15` (`classify_subfolder`), `:37` (`sanitize_folder`) | AI tematyczne filowanie → nazwa subfolderu; sanitizer też reużywany przez folder-commands | **library** (auto-filing), `commands.rs:2689,3471` | `complete()` |
| **brief** | `:7` (`build_brief_prompt`) | Pre-meeting brief (Context/Still-open/Talking-points) z past-notatek | **record** (pre-meeting), `commands.rs:1674` | `complete()` |
| **digest** | `:10` (`build_digest_prompt`) | Tygodniowy digest kilku spotkań w 1 notatkę (themes/decisions/open-items/meetings) | **analytics/library**, `commands.rs:1519` | `complete()` |
| **dossier** | `:106` (`build_dossier_data`), `:177` (`dossier_system_prompt`), `:255` (`render_dossier_user`), `:267` (`format_dossier_client`) | Flagowy cross-meeting synthesis o JEDNEJ encji (Overview/🕑Timeline/⏳Commitments/🧭Next) | **graph** (cloud `entity_dossier`) **I MCP** (`get_entity_dossier`) | `complete()` LUB egress-free |

## 4. Krytyczne ustalenia (nie certyfikuję na ślepo)

1. **Dwie powierzchnie dossier, jeden gated builder** (`dossier.rs:9-18`): cloud `entity_dossier` (firewall+zgoda, `commands.rs:1450-1458`) vs **MCP `get_entity_dossier` — EGRESS-FREE**: zwraca strukturalne dane do lokalnej syntezy klienta (Claude Desktop), serwer MCP NIGDY nie woła chmury (`mcp.rs:661`, test `:663`). `format_dossier_client` (`:267`) bez provider-calla.

2. **Bramki widoczności (anti-leak) są UPSTREAM redakcji.** Każdy korpus egresujący do chmury (related_context, vault_context, dossier) przechodzi `search_visible`/`get_note_if_visible`/`entity_*_visible` na żywym secie `unlocked` — sealed-and-not-unlocked notatka NIE wnosi nic (tytuł/treść/commitment). Testy RED-if-ungated: `vault_context.rs:211`, `related_context.rs:324`, `dossier.rs:346`. To zgodne z lock-model.

3. **Feature-gating (realnie podłączone vs wyłączone):**
   - Firewall regex (email/card/phone), bramka zgody, RedactingProvider, wszystkie 3 providery, wszystkie mózgi: **REALNIE WIRED** w default build.
   - related_context (Phase-4 RAG, FTS-only): **ALWAYS-ON**, bez flagi (`pipeline.rs:489`).
   - **Redakcja nazwisk: WYŁĄCZONA domyślnie** — Noop; realny NER tylko pod `--features local-ner` + pobrany model. W default-build **imiona egresują do chmury**.
   - **Hybrid/semantic Ask** (`build_vault_context_hybrid_visible`): bramkowany podwójnie — `config.semantic_search_enabled` (default off, `commands.rs:1390`) **i** realny embedder tylko pod `--features local-embed` (inaczej StubEmbedder). Default → ścieżka FTS-only.
   - `local-brain` (`orchestrate::orchestrate_context`, `pipeline.rs:517`): z prawdziwym reasonerem planuje retrieval; default = **StubReasoner** → spada do deterministycznego `build_grounding_context` (byte-identyczne).

4. **Uczciwy bar:** Touch ID/lock-at-rest/screen-share weryfikują się tylko na podpisanym buildzie; recall NER dla polskiego = niezweryfikowany (eval @Mac). Realne wywołania cloud (claude CLI / Anthropic HTTP) nie da się zweryfikować w `cargo test --lib` — testy używają Echo/Capture/Fixture stubów.

Pliki kluczowe: `summarize/provider.rs`, `summarize/mod.rs`, `summarize/{claude_code,anthropic,ollama}.rs`, `summarize/redact.rs`, `summarize/template.rs`, `summarize/{related_context,vault_context,dossier}.rs`, `settings/config.rs`, `pipeline.rs:485-545`, `commands.rs` (call-sites wyżej), `mcp.rs:332,657`.

## R4 — Realtime agentic AI actions

Mam pełny obraz. Zwracam ustalenia.

---

# R4 — REALTIME AGENTIC AI ACTIONS (pełny obraz)

## TL;DR — co realnie działa
Realtime asystent głosowy (in-meeting) jest **w pełni podłączony** do żywej pętli transkrypcji i działa end-to-end: wake → intent → gated RAG (vault + web + kalendarz) → odpowiedź z cytowaniami → event do FE. Mózg domyślnie = **Cloud** (istniejący provider, ten sam firewall redakcji + bramka zgody co notatka). Lokalny mózg/embedder/NER to **wyłączone domyślnie feature-gate'y** (`local-brain`/`local-embed`/`local-ner`) — bez nich `active_reasoner` z backendem `Local` schodzi do `StubReasoner`. Dwa connectory zewnętrzne: **web (Brave, BYO key, consent-gated, fail-closed)** i **kalendarz (lokalny, on-device, BEZ zgody)**. Realtime dispatch jest **domyślnie OFF** (toggle `realtime_reactions=false`), ale ścieżka manualna „Ask AI" działa zawsze podczas nagrania. Slack = stub.

---

## 1. Pętla agentowa i rejestr narzędzi (`tools.rs`)

**Jeden bramkowany seam `execute_tool`** — `ToolCall` ma 8 wariantów (`tools.rs:33-61`):
- 6 narzędzi **vault** (egress-free, czyta lokalne SQLite, każde gejtowane na `unlocked`): `SearchMeetings`, `GetMeeting`, `ListRecentMeetings`, `SearchSemantic`, `GetOpenCommitments`, `GetEntityDossier` (`tools.rs:75-203`). Każda gałąź idzie przez `search_visible`/`search_hybrid_visible`/`meeting_is_visible`/`get_note_if_visible`/`list_open_commitments`/`build_dossier_data` — sealed-not-unlocked nigdy nie wypływa.
- 2 **connectory** (`WebSearch`, `CalendarLookup`) są **celowo odrzucane** przez synchroniczny `execute_tool` (`tools.rs:180-202`, zwraca `AppError::InvalidArg`) — bo `execute_tool` to jedyne wejście powierzchni MCP i MUSI być egress-free. Dispatch tylko async: `execute_web_search` (`tools.rs:224`) i `execute_calendar_search` (`tools.rs:260`).
- `SearchSemantic` jest **dodatkowo** gejtowane flagą `config.semantic_search_enabled` (domyślnie `false`, `config.rs:223`) — przy OFF zwraca jawny string „disabled", NIE cichy ungated read (`tools.rs:89-94`).

**Jak model „wybiera" narzędzia — dwie różne pętle:**

### Flow A — grounding NOTATKI (`orchestrate.rs`, NIE realtime)
To pętla przy *podsumowaniu* po spotkaniu. Reasoner dostaje wycinek 2000 znaków (`orchestrate.rs:44`), zwraca **plan retrievalu** wg schematu JSON (`pre_analysis_schema`, `orchestrate.rs:59-82`: `entities/topics/retrieval_queries[{tool,query}]`, max 4). Każde query → `map_to_tool_call` → `execute_tool` → cited corpus dla `SummarizeRequest.related_context` (`orchestrate.rs:240-290`). **Stub-shim/floor:** gdy `reasoner.id()=="stub"` spada na deterministyczny `build_grounding_context` byte-identical (`orchestrate.rs:123-124`); każdy błąd/parse-fail/pusty corpus też (`orchestrate.rs:132-157`). Self-exclude meeting po `id:<id>` (`orchestrate.rs:250,270-275`). Pod domyślnym backendem **Cloud** `reasoner.id()` ≠ "stub" (`CloudReasoner.id()` zwraca `self.id`, `reason.rs:509-511`), więc ścieżka mózgu Flow A **realnie się odpala**.

### Flow B — REALTIME voice action (`voice_action.rs`, rdzeń pytania)
To NIE jest planująca pętla wielokrokowa — to **deterministyczny fan-out**: intent → uruchom WSZYSTKIE odpowiednie legi (vault FTS + semantic + dossier dla recall + web dla research + kalendarz gdy intent kalendarzowy) → zbierz gated grounding → JEDNO wywołanie mózgu do syntezy 2-4 zdań (`voice_action.rs:257-509`). „Wybór narzędzi" jest regułowy, nie LLM-owy:
- `handle_voice_action` mapuje `VoiceIntent` na akcję (`voice_action.rs:108-161`): Research/Recall → `rag_answer`; CreateReminder → `add_reminder_blocking`; NoteAside → `note_aside` (gated, additive); SlackSearch → **`"unavailable"` (stub)**; Unknown → „unrecognized".
- `rag_answer` (`voice_action.rs:257`): buduje queries **literal-first** (`retrieval_queries`, `voice_action.rs:234-248`) — kluczowy fix cross-lingual: vault FTS jest exact-term, więc retrieval keyuje na DOSŁOWNYCH słowach użytkownika (PL „pogoda"), nie na przetłumaczonym przez mózg topicu. Mózg-topic to dodatkowy leg, nie zamiennik. (Regresja end-to-end: `voice_action.rs:1196-1273`.)

---

## 2. Połączenie realtime: transkrypt na żywo → reasoning → tool call → wynik z cytatami (`transcribe/live.rs`)

To jest spoiwo „realtime". Pętla live captions tika co kilka sekund (profil Fast/greedy, `live.rs:154-158`) i na każdym ticku:

**A) Ścieżka WAKE (automatyczna, gated toggle'em):**
1. `wake_event_for(&text)` + dedup `wake_dedup.should_fire` (`live.rs:222-223`) — rolling ~14s okno nakłada się, więc dedup składa powtórki tego samego „Klaudku" w jeden dispatch.
2. `detect_wake` (`wake.rs:74-92`) — fuzzy/fonetyczny matcher wołacza („Klaudku/Klauku", shape-gate `kl…`+rdzeń samogłoskowy+`-ku/-ko`, `wake.rs:128-155`); odpala **anywhere** w oknie (fix #23 recall). `parse_voice_intent` (`wake.rs:273-367`) — deterministyczny PL+EN keyword parser → `VoiceIntent` (`wake.rs:255-268`).
3. **Bramka dispatchu:** `should_dispatch(config) = config.realtime_reactions` (`live.rs:270-272`) — **domyślnie `false`** (`config.rs:227`). OFF = tylko SURFACE wake'a (event `EVENT_WAKE_DETECTED`), bez akcji. ON = `spawn_dispatch` na **odłączonym wątku** (`live.rs:253-256, 286-334`) — bo cloud call trwa sekundy i NIE wolno mu blokować ticku/captionu.
4. `spawn_dispatch` snapshotuje config + **live `unlocked` set** + current meeting → `handle_voice_action(..., Some(&app))` (`live.rs:311-320`) → `persist_interaction` (Q&A do DB) → `emit EVENT_VOICE_ACTION_RESULT`.

**B) Ścieżka MANUALNA „Ask AI" (CLICK-TO-STOP, niezależna od toggle'a):**
- Sprawdzana KAŻDY tick (`step_manual_capture`, `live.rs:176-209, 462-491`). Po kliknięciu transkrybuje **izolowane** okno post-click (`transcribe_since`, `live.rs:497-535`), akumuluje aż do „stop"/backstopu, wymusza język klipu (`resolve_clip_lang`, bo 3s klip Whisper myli PL→RU), odrzuca filler/garbage (`is_meaningful_command`, `live.rs:607-619`).
- Na dispatch: `spawn_command_dispatch` (`live.rs:378-428`) → `resolve_command_intent` (keyword → **brain interpret** `interpret_with_brain` → fallback Research, `live.rs:680-694`) → `handle_voice_action` z dosłownym `command` jako literal → `.with_command()` (FE pokaże „usłyszano: …").

**Cytaty (grounding „z cytatami"):** `rag_answer` buduje `citations` z TRZECH źródeł, wszystkie tylko z VIDOCZNYCH meetingów (`voice_action.rs:406-452`):
- vault: ponowny gated `db.search_visible` → `[[Title]]` wikilinki (`voice_action.rs:418-433`);
- web: linie `- [web · Brave] …` → `(web) Title — url` (`web_citation_from_line`, `voice_action.rs:616-640`);
- kalendarz: linie `[calendar] …` → `(calendar) Title` (`calendar_citation_from_line`, `voice_action.rs:603-612`).
Prompt syntezy każe cytować vault przez `[[Title]]` i atrybuować web jako „(via web)" (`voice_action.rs:466-470`).

---

## 3. Connectory jako on-demand narzędzia + CONSENT-GATED egress (`connectors/`)

**Framework (`connectors/mod.rs`):** `EgressClass::{Local,External}` (`mod.rs:38-43`); trait `Connector` (`mod.rs:113-123`); `ConnectorRegistry::build(config)` eksponuje connector **tylko** gdy spełnione bramki (`mod.rs:141-147`). Trzy load-bearing dyscypliny (`mod.rs:12-24`):
1. **Consent-gated, fail-closed** — `External` widoczny tylko gdy enabled+consented(+keyed). Nie-eksponowany id → `ConnectorRegistry::search` zwraca `NeedsConsent` **bez dotknięcia sieci** (`mod.rs:164-168`).
2. **Redacted** — `ConnectorRegistry::search` przepuszcza query przez `summarize::redact::redact` PRZED connectorem (`mod.rs:172-173`), więc pojedynczy connector nie może zapomnieć. (Test: email → token `⟪EMAIL_…⟫` zanim connector zobaczy, `mod.rs:213-230`.)
3. **Loud** — każdy `ConnectorHit` ma `source_label` (np. „web · Brave"), nigdy pusty.

**Web (`connectors/web.rs`) — JEDYNY External / nowa klasa egresu:**
- `WebConnector::from_config_if_available` (`web.rs:172-184`): zwraca `Some` **TYLKO** gdy `web_search_enabled && web_search_consented && Brave key w Keychain` (`WEB_SEARCH_KEY_ACCOUNT="web_search_api_key"`, `web.rs:35`). Brak czegokolwiek → `None` → connector nieobecny w rejestrze → mózg nie dostaje narzędzia (fail-closed). Wszystkie defaulty `false` (`config.rs:228-229`).
- Sub-seam `WebSearchProvider` (`web.rs:46-54`), domyślnie `BraveSearch` (GET `api.search.brave.com/res/v1/web/search`, header `X-Subscription-Token`, `web.rs:106-129`). Key BYO z Keychain, nigdy logowany; `e.without_url()` ściąga query z błędu reqwest (no-PII, `web.rs:117`).
- Bramka zgody: command `consent_to_web_search` (`commands.rs:1808-1816`) → `grant_web_search_consent` (preserve-only flaga, jedyny sposób flipnięcia na true; DTO jej NIE ustawia — test `commands.rs:4958-4977`). FE: `ipc.consentToWebSearch()` (`ipc.service.ts:162`).

**Kalendarz (`connectors/calendar.rs`) — Local, BEZ zgody:**
- `EgressClass::Local` (`calendar.rs:84-87`) → świadomie NIE consent-gated, bo czyta on-device. Stateless: dostaje już-pobrany `Vec<CalendarEventFull>`, sam nie dotyka EventKit ani AppHandle (`calendar.rs:36-48`) — to trzyma `ConnectorRegistry` AppHandle-free.
- Match po title/attendees/notes (`event_matches`, `calendar.rs:53-63`); pusty query = wszystkie eventy w oknie (`calendar.rs:89-102`).
- Realny fetch: `crate::calendar::fetch_events` (top-level `calendar.rs`) driuje **bundled sidecar `meetnotes-calendar`** (EventKit, osobny proces, watchdog, `SIDECAR_TIMEOUT=10s`, `calendar.rs:31-33`), degraduje do pustego `Vec` na KAŻDY błąd (brak sidecara/denied TCC/timeout/zły JSON). Okno `[now-60m, now+720m]` (`tools.rs:257`, `voice_action.rs:263`).
- Leg kalendarza jest **intent-gated** w realtime: `wants_calendar` (bilingwalny keyword set EN/PL, `voice_action.rs:564-575`) — odpala tylko na pytania kalendarzowe, żeby „jaka pogoda" nie wciągało szumu. Wymaga AppHandle (w headless testach po prostu pomijany).

**Egress realtime w sumie:** vault legi = zero egresu; web leg = redacted+consented; synteza mózgu Cloud = ten sam `make_provider` (bramka zgody + RedactingProvider) co notatka — `Unavailable` (brak zgody) → status `needs_consent`, łaskawie, bez wycieku, z gated cytatami (`voice_action.rs:491-500`).

---

## 4. Mózg/reasoner — co wired, co feature-gated, co stub

`active_reasoner(config)` dispatch po `brain_backend` (`reason.rs:258-272`), default **Cloud** (`#[default] Cloud`, `config.rs:24-25`):
- **Cloud** → `CloudReasoner` przez `make_provider` (zero nowej klasy egresu, `reason.rs:260-265`). Konstrukcja tania/infallible, zgoda fire'uje przy CALL-time.
- **Local** → `local_reasoner`: realny `MistralReasoner` **TYLKO** pod `--features local-brain` ORAZ obecny GGUF; inaczej `StubReasoner` (`reason.rs:278-289`). Feature **OFF by default** (`Cargo.toml:55,138` — `local-brain=["dep:mistralrs"]`, default `[]`). `reason/mistral.rs` istnieje, ale niekompilowany domyślnie.
- **Off** → `StubReasoner` (deterministyczny floor).
- `interpret_with_brain` (`voice_action.rs:176-221`) — mapuje free-form komendę na intent przez `reasoner.structured` (schema action+argument), best-effort, `None` przy braku/no-consent.

Powiązane (też OFF default): `local-embed` (CandleBert e5, inaczej `StubEmbedder`, `embed.rs`), `local-ner` (Deberta name-redactor, inaczej Noop). To są **szkielety za feature-flagami** — w domyślnym buildzie egzekwują floor.

---

## 5. FE — jak realtime akcje i ich źródła są pokazywane

**Live card (`record/assistant-actions.component.ts`):** in-flow `.card` (świadomie NIE floated → frosted OK, trap T3 nie dotyczy, komentarz `:24-27`). `AiOrb` + lista newest-first z `AssistantStore`. Pending row „🎙 usłyszano: {command}" → „Thinking…" → odpowiedź. Status pille: ok/needs_consent/unavailable/unrecognized/nothing_heard/error (`:236-272`).

**Store (`core/assistant.store.ts`):** root singleton, subskrybuje RAZ 4 strumienie eventów (`onWakeDetected`/`onVoiceActionResult`/`onVoiceCommandListening`/`onVoiceCommandProcessing`, `init()` idempotentny + concurrency-guard), payloady → signale (zgodne z zakazem subscribe-into-field). `orbState` = pure `computed` (idle→listening→processing→answer, brak NG0600). `parseCitations` dzieli flat `string[]` na `{kind:'vault'|'web', label, url}` — `(web) Title — https://…` vs `[[Title]]` (`:60-79`).

**Detail Q&A (`detail/detail.component.ts`):** persystowane interakcje (`insert_assistant_interaction` w live.rs:352) renderowane jako sekcja Q&A; `parseCitation` (`:2831-2857`) parsuje 5 form (bare URL, `[[Title]]`, `(web)` bez URL, `Label (url)`, fallback vault). Derived convenience — purgowane na seal.

**Wake-anchor consent UX (ostatnie commity, `record.component.ts`):** banner „cloud-egress consent" zamiast cichego faila (`:283-301`); `consentBlocked` wykrywa marker „cloud egress not consented" z `make_provider` (`:1251-1258`) → przycisk „Allow & finish note" → `consentToCloudEgress()` + retry resummarize (`:1494-1509`). Card pokazywana gdy `realtimeReactions===true && brainBackend!=='off'` LUB manualny „Ask AI" in-flight (`:1209-1219`). „Ask AI" = CLICK-TO-STOP toggle `askNow()/endAsk()` (`:1471-1486`).

---

## 6. ŚWIEŻE / EKSPERYMENTALNE (uncommitted + untracked) — to jest „handoff layer"

`git status`: 2 zmodyfikowane + 2 untracked. To **refactor prezentacji cytowań**, nie zmiana logiki:
- **NOWY untracked plik** `src/app/shared/assistant-sources.component.ts` — wspólny blok „🔗 Źródła": dedupe (URL dla web / label dla vault), domain extraction (`new URL().hostname` bez www), cap PREVIEW=4 + toggle „Pokaż wszystkie (N)" (signal + `@if`), web row = klikalny external link, vault row = distinct chip (`:212-269`). Zastępuje stary „giant flat list of VIA WEB chips".
- **Uncommitted `assistant-actions.component.ts`**: usuwa inline cite-chips, dodaje `app-markdown` (marked→DOMPurify→innerHTML, sanitized) dla odpowiedzi + `<app-assistant-sources [citations]>` (diff: -92 linii stylów/template).
- **Uncommitted `detail.component.ts`**: identyczny refactor Q&A — `app-markdown` dla `q.answer` (zamiast `<p>{{}}</p>`) + `app-assistant-sources` zamiast ręcznych `.qa-cites` (diff: -99 linii).
- **Untracked `src-tauri/binaries/`** — prawdopodobnie zbundlowane sidecary (audio/kalendarz); niezacommitowane, relevantne dla działania kalendarza/audio na realnym buildzie.

Status tych zmian: **niezweryfikowane bramkami** (brak commita), ale to czysto prezentacyjny refactor reużywający istniejących, otestowanych komponentów (`MarkdownComponent`). Wymaga `ng lint`/`ng build` (budżet 16kB/komponent) przed certyfikacją.

---

## 7. Uczciwy bilans (czego NIE wolno certyfikować na ślepo)
- **Realtime dispatch domyślnie OFF** (`realtime_reactions=false`) — bez włączenia w Settings wake tylko surface'uje, nie działa. Manualne „Ask AI" działa zawsze podczas nagrania.
- **Web search domyślnie niedostępny** — wymaga enable + one-time consent + BYO Brave key; inaczej connector nieobecny (fail-closed, zero egresu).
- **Lokalny mózg/embedder/NER = wyłączone feature-gate'y** (`local-brain`/`local-embed`/`local-ner`); domyślny runtime brain = **Cloud**, domyślny build local = **StubReasoner/StubEmbedder**. To realne szkielety, nie atrapy bez kodu, ale OFF.
- **Slack = stub** („unavailable", `voice_action.rs:150-154`).
- **Niezweryfikowane headless** (wymaga signed build + realny Mac): precyzja wake na realnym mikrofonie (`wake.rs:18-21`), latencja cloud round-trip, EventKit zwracający realne eventy + TCC Calendars (`calendar.rs:`/`voice_action.rs:25`), Touch ID. `cargo test --lib` pokrywa parser/gating/dispatch z mock reasonerem + seeded gated data.
- **Bezpieczeństwo locka — wygląda solidnie**: każdy read realtime idzie przez `execute_tool`/`search_visible` na LIVE `unlocked` set; testy RED-before-GREEN dowodzą, że sealed-not-unlocked nie wpływa do groundingu/cytowań ani mózgu (`voice_action.rs:846-906`, `orchestrate.rs:466-522`); NoteAside refuse na sealed (`voice_action.rs:962-980`). Mimo to: lock-touching → wymaga `lock-security-reviewer` przed merge.

**Kluczowe pliki:** `/Users/jakubgawronski/Projects/meetnotes/src-tauri/src/{tools.rs,orchestrate.rs,voice_action.rs,reason.rs,calendar.rs}`, `/Users/jakubgawronski/Projects/meetnotes/src-tauri/src/connectors/{mod.rs,web.rs,calendar.rs}`, `/Users/jakubgawronski/Projects/meetnotes/src-tauri/src/audio/wake.rs`, `/Users/jakubgawronski/Projects/meetnotes/src-tauri/src/transcribe/live.rs`, `/Users/jakubgawronski/Projects/meetnotes/src/app/core/assistant.store.ts`, `/Users/jakubgawronski/Projects/meetnotes/src/app/shared/assistant-sources.component.ts` (untracked), `/Users/jakubgawronski/Projects/meetnotes/src/app/features/record/assistant-actions.component.ts` (uncommitted), `/Users/jakubgawronski/Projects/meetnotes/src/app/features/detail/detail.component.ts` (uncommitted).

## R5 — Lock model + MCP + egress

Mam komplet dowodów. Poniżej zwięzłe ustalenia.

---

# R5 — Lock model + powierzchnia MCP + egress (Murmur, kod @ branch `murmur`)

Werdykt: **model prywatności jest mocny i spójny — verify-before-destroy oraz gating są realnie wszędzie, a powierzchnia MCP jest read-only, loopback-only i wizyjnie bramkowana.** Dwie istotne luki/zastrzeżenia (token MCP domyślnie OFF, redakcja nazwisk domyślnie no-op) — opisane na końcu. Wszystko poniżej zweryfikowane na żywym kodzie.

## 1. Lock model — dwie warstwy szyfrowania

**Warstwa 1 (DEK, cały plik):** SQLCipher na całej bazie. DEK to 64-hex z keychaina, item **PLAIN, NIE biometryczny** — celowo, bo czytany przy każdym starcie (`keychain.rs:43-46`, `get_or_create_db_dek` `:46`). Chroni at-rest gdy apka zamknięta; po starcie cała baza jest czytelna w sesji.

**Warstwa 2 (CK per-folder, wrapowany KEK):** AES-256-GCM. Lock folderu generuje losowy CK (`commands.rs:3045`), wrapuje go masterem KEK (`:3048`). KEK jest **biometryczny**: keychain item z `kSecAccessControlUserPresence` (Touch ID lub fallback passcode) + `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` (`keychain.rs:405-408`). Kluczowe: gate jest **OS-owy (kSecAttrAccessControl), nie app-owy** — flaga `lock_require_biometric` jest tylko informacyjna i nie potrafi go ominąć (`commands.rs:3122-3127`).

**AAD context-binding (B7/B8) — mocna strona:** każdy blob jest AEAD-związany ze swoim kontekstem składowania: wrapped-CK→`folder` (`aad_wrapped_ck` `commands.rs:3795`), content→`folder|meeting|provider|type|v1` (`aad_content` `:3802`), audio→`meeting|folder|stream-role` (`aad_audio_role` `:3851`). Swap ciphertextu między folderami/spotkaniami/strumieniami **fails closed** z `AppError::Locked` (`crypto.rs:89-92`, test `swapped_context_blob_fails_closed` `crypto.rs:239`). Backward-compat: legacy puste-AAD bloby nadal się odszyfrują (fallback `crypto.rs:84-88`, flaga `AadUsed::Legacy` → re-bind), więc istniejące foldery nie są brickowane.

## 2. Verify-before-destroy — POTWIERDZONE na każdej ścieżce seala

- **Notatka:** `lock_folder_inner` szyfruje markdown każdego (meeting,provider) i **dekoduje z powrotem + porównuje bajtowo PRZED** `seal_note` (`commands.rs:3060-3066`); dopiero potem `seal_note` (`db.rs:1769`) zeruje kolumnę `markdown=''`. Per-provider — nie kolapsuje do jednego bloba (chroni przed content-loss bug).
- **Transkrypt/timeline:** `seal_meeting_extras` weryfikuje decrypt każdego segmentu (`commands.rs:4002`) i timeline (`:4018`) zanim `seal_segment`/`seal_timeline` zblankuje plaintext.
- **Audio:** `encrypt_file` dekoduje własny output i asertuje byte-identical **w środku, przed zwrotem** (`crypto.rs:124-131`); dopiero potem caller usuwa plaintext WAV. Round-trip test `crypto.rs:293`.
- **Kolejność crash-safe:** flaga `locked`+wrapped key → seale blobów → seal extras → purge wektorów → **usunięcie vault `.md` NA KOŃCU** (`commands.rs:3100-3104`), bo zgubiony `.md` jest odtwarzalny, zgubiona treść nie.
- **Startup reconciliation:** `reblank_locked_folders_at_rest` (`db.rs:1871`) re-asercja sealed-shape — blankuje tylko rzędy które MAJĄ blob (źródło prawdy), nigdy nie niszczy nieosealowanej treści; zwraca ścieżki audio (3 strumienie) do re-seala stray plaintextu (B1).

## 3. Każdy read GATED — potwierdzone

- **Komenda detalu:** `get_meeting_detail` → `meeting_is_unlocked` (`commands.rs:2163`) → `masked_detail` gdy locked: `title="🔒 Locked"`, `note=None`, `segments=[]`, `assistant_interactions=[]`, **`audio_path=None`** (`:2204-2218`). `meeting_is_unlocked` (`:4198`) sprawdza folder→locked→czy w sesyjnym secie `unlocked_folders`.
- **convertFileSrc / asset leak — ZAMKNIĘTY:** zerowanie `audio_path` w masked DTO (`:2208`) jest jedyną rzeczą zamykającą ścieżkę `asset:`/`convertFileSrc`, która omija `export_audio` + gate (rationale `:2155-2162`; test `masked_detail_nulls_audio_path...` `:4414`). `export_audio` (`:757`) i `export_master` (`:789`) też bramkowane + nigdy nie oddają ścieżki do FE.
- **Warstwa DB (MCP/graph/search):** `visibility_clause` (`db.rs:3056`) = `(f.locked IS NULL OR f.locked=0 OR f.id IN (unlocked))`, wpięty w `search_visible` (`:2191`), `list_meetings_visible` (`:2270`), `get_note_if_visible` (`:2309`), `meeting_is_visible` (`:2330`), `list_entities_visible` (`:2610`), `search_hybrid_visible` (`:1007` — gating na WSZYSTKICH trzech nogach: FTS + wektor + graph). Defense-in-depth: tekst notatki jest też purge'owany z `fts_notes` przy blankowaniu, więc sealed nie wygeneruje nawet kandydata (`:2207-2210`).

## 4. Unlock / relock / remove — odwracalne, biometric-gated, race-safe

- `unlock_meeting` (`commands.rs:3748`) → resolve folder → `unlock_folder` (`:3112`). Jeden prompt Touch ID (odczyt biometrycznego KEK na `spawn_blocking` `:3158`), KEK cache'owany na sesję by nie re-promptować; decrypt notatek/transkryptu/timeline + materializacja grywalnego WAV; dodanie folderu do `unlocked_folders`.
- **Race-safety (BLK-1):** `lifecycle_guard` (`Mutex<()>`, poison-recoverable `:3025`) serializuje całą maszynę stanów; `remove_lock_inner` trzyma guard przez CAŁY restore→clear (`:3331`), więc off-threadowy `relock_all_inner` nie wblankuje `markdown=''` między Step1 (restore) a Step2 (clear blob) — dokładnie ten permanent-loss race.
- `relock_all_inner` (`:3278`): czyści set, **zeroizuje KEK** (`zeroize::Zeroize` `:3297`, nie hand-loop), re-blankuje, **WAL checkpoint TRUNCATE** by plaintext nie został w sidecarze (`:3311`, B12).
- `remove_lock_inner` (`:3330`): restore KAŻDEGO provider-row PRZED czyszczeniem jakiegokolwiek bloba (`:3358-3370`), re-eksport `.md`, decrypt audio `.enc`→plaintext. Never lose audio.

## 5. MCP — 6 narzędzi, read-only, loopback, wizyjnie bramkowane

Pełna lista (`mcp.rs:230-262`): **search_meetings, get_meeting, list_recent_meetings, search_semantic, get_open_commitments, get_entity_dossier** (test `tools_list_has_six_tools` `:374`). **Zero write tools.**

Zabezpieczenia warstwy transportu:
- **127.0.0.1:8765 only** (`MCP_PORT:16`, bind `:66`).
- **Host allowlist** dokładnie `{127.0.0.1:8765, localhost:8765}`, brak/inny Host → 403 (`:25`, `:102-108`) — blokada DNS-rebinding.
- **Origin allowlist** gdy obecny; cross-origin/`null` → 403, nigdy nie reflektuje (`:31-39`, `:112-118`).
- **Bearer token fail-closed:** gdy `require_token` a token nie da się zmintować → **serwer odmawia startu** (nie degraduje do otwartego, `:75-89`); gdy ON, token wymagany **przed KAŻDĄ metodą** włącznie z initialize/tools/list/ping (`:178-182`), porównanie **constant-time** (`bearer_ok` `subtle::ConstantTimeEq` `:206-220`).
- Body cap 1 MiB (`:20`), tylko POST.

Bramkowanie treści: każdy `tools/call` idzie przez `handle_tool_call`→snapshot `unlocked_set` (`:289`)→`dispatch_tool`→jeden gated seam `crate::tools::execute_tool` (`tools.rs:69`). Sealed-not-unlocked jest niewidoczne dla MCP, bo wszystkie readery używają `visibility_clause`/`*_visible`. Trzy testy to dowodzą: `search_semantic_is_visibility_gated...` (`mcp.rs:542`), `get_open_commitments_is_visibility_gated` (`:592`), `get_entity_dossier_is_visibility_gated_and_egress_free` (`:663`) — każdy: sealed nie wycieka, po session-unlock wraca.

**Egress MCP = ZERO** (`tools.rs:16-18`): `execute_tool` czyta tylko lokalne SQLite + lokalny embedder; `WebSearch`/`CalendarLookup` są **odmawiane** w synchronicznej ścieżce (`tools.rs:180-202`, testy `:385`,`:424`) — to jedyne wejście MCP, więc MCP nie może sięgnąć konektora.

## 6. Egress — gdzie cokolwiek opuszcza urządzenie

- **Cloud LLM (`anthropic`, `claude_code`):** `make_provider` (`summarize/mod.rs:64`) — **consent-gate fail-closed** `cloud_egress_consented` (`:70-76`), potem **RedactingProvider owija OBA** cloud providery (`:124-129`); `ollama` (lokalny) zwraca unwrapped wcześniej (`:101-106`). `claude_code` shelluje lokalny CLI, ale CLI uploaduje do Anthropic → też za firewallem (`:114-117`). Anthropic → `api.anthropic.com`, min TLS 1.2 (`anthropic.rs:82-83`).
- **Redakcja:** regex firewall (email/karty/telefon) zawsze; warstwa **nazwisk (NER) jest feature-gated `local-ner` i DOMYŚLNIE NO-OP** (`redact.rs:78-92`, `:222-230`) — patrz luki.
- **Connector web:** `EgressClass::External`, eksponowany do brain TYLKO gdy `web_search_enabled && web_search_consented && klucz obecny` (`connectors/mod.rs:141-146`); rejestr **redaguje query PRZED** wywołaniem konektora (`:172-173`); nie-eksponowany → `NeedsConsent`, **nic nie wychodzi** (`:164-168`). Brave API (`web.rs`). Consent `web_search_consented` jest preserve-only, flipowany dedykowaną komendą (`settings/config.rs:185`, `:506`).
- **Connector kalendarz:** `EgressClass::Local` — on-device EventKit sidecar, **NIE consent-gated** (`connectors/calendar.rs:84-86`), graceful-degrade do pustego Vec.
- **Inbound-only:** `redact.rs:124` `reqwest::get` to pobieranie modelu NER (przychodzące), nie wyciek.

## Mocne strony
- Verify-before-destroy faktycznie wszędzie (notatka/transkrypt/timeline/audio), z testami round-trip.
- AAD context-binding defeats swap/replay (folder↔folder, meeting↔meeting, mic↔sys↔playback).
- Podwójne bramkowanie (komenda `meeting_is_unlocked` + DB `visibility_clause`).
- Asset-path/`convertFileSrc` leak realnie zamknięty zerowaniem `audio_path` w masked DTO.
- KEK biometryczny egzekwowany przez OS (nie da się obejść flagą), zeroizowany na relock, WAL truncate.
- MCP: read-only, loopback, Host/Origin/token, jeden gated seam dzielony z przyszłym brain.
- Egress: jeden firewall (`make_provider` + `ConnectorRegistry::search`) — pojedyncza ścieżka, której konektor „nie może zapomnieć".

## Luki / zastrzeżenia (mówię wprost)
1. **Token MCP domyślnie OFF** (`spawn(..., require_token)`; `mcp.rs:411` test „token_disabled_keeps_discovery_open"). Gdy OFF, **dowolny lokalny proces** może wołać 6 narzędzi bez auth. Łagodzące: loopback-only + wszystko visibility-gated (sealed bezpieczne) + read-only — ale **cała treść ODBLOKOWANYCH spotkań jest czytelna bez uwierzytelnienia** dla każdego procesu na maszynie. To główna powierzchnia ataku MCP; warto rozważyć domyślne ON.
2. **Redakcja nazwisk domyślnie no-op** (`redact.rs:222-230`). W standardowym buildzie (bez `local-ner` + modelu) **imiona/nazwiska osób WYCHODZĄ do chmury** — firewall regex łapie tylko email/karty/telefon. To świadomy feature-gate, ale realny default-gap dla `anthropic`/`claude_code` i redagowanego web-query.
3. **`visibility_clause` buduje SQL przez interpolację stringów** id-ów folderów (escape `'`→`''`, `db.rs:3061`), nie parametryzowane. Id-y są wewnętrzne (uuid), więc praktycznie bezpieczne, ale to smell — gdyby kiedyś id folderu stało się user-controlled, byłby wektor SQLi.
4. **DEK jest itemem PLAIN** czytanym przy starcie — po uruchomieniu apki cała baza jest odszyfrowana w sesji; jedyną ochroną treści zablokowanych folderów w działającej apce jest warstwa CK/KEK. To udokumentowany design (`keychain.rs:43-46`), nie bug, ale warto pamiętać: lock-at-rest dla całości działa tylko gdy apka zamknięta.

Honestly-flagged: biometryka, screen-share auto-relock i realne zachowanie Touch ID weryfikują się tylko na **podpisanym buildzie** (w dev/unsigned `biometric` degraduje do `Ok(true)`); powyższe to weryfikacja statyczna kodu + testów `--lib`, nie dowód runtime na podpisanym buildzie.

Kluczowe pliki: `src-tauri/src/crypto.rs`, `src-tauri/src/secrets/keychain.rs`, `src-tauri/src/storage/db.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/mcp.rs`, `src-tauri/src/tools.rs`, `src-tauri/src/connectors/mod.rs`, `src-tauri/src/summarize/mod.rs`, `src-tauri/src/summarize/redact.rs`.


---

# Załącznik B — Surowy research konkurencji (3 agenci)

## C1 — Konkurencja meeting-notes/AI

# Research: Konkurencja w meeting-notes/AI — „budowanie kontekstu" i MCP (zadanie C1)

## Verdict
Do połowy 2026 **MCP i „ask-across-all-meetings" przestały być wyróżnikiem — są table-stakes**: Granola, Otter, Fireflies, Fathom, tl;dv, Circleback i Limitless mają już serwer MCP i cross-meeting chat. Ale wszystkie ich serwery MCP to **hostowane/chmurowe** endpointy nad danymi, które i tak leżą w ich AWS — Murmur jest praktycznie jedyny z **lokalnym, loopback-only MCP nad on-device SQLite**. Prawdziwa przewaga Murmura to nie „ma MCP", tylko „MCP + RAG + posiadane pliki bez wysyłania spotkań do czyjejkolwiek chmury". Szczera słabość: chmurowi gracze (zwłaszcza Granola i Otter) budują „ogromny kontekst" **dziś większy i szerszy** — auto-capture całych zespołów, miesiące historii i wpinanie źródeł cross-app (Gmail/Salesforce/Notion/Slack/Jira), czyli dokładnie wizja brain2 — i są tam wyprzedzeni względem Murmura w zakresie.

## What we already have (grounded)
Z kontekstu projektu (nie re-greppowałem kodu dla tego czysto-zewnętrznego kąta — confidence med):
- **Lokalny read-only MCP** na `127.0.0.1:8765` (`src-tauri/src/mcp.rs`), narzędzia czytają lokalny SQLCipher i są bramkowane przez `visibility_clause` — nic nie wychodzi z maszyny.
- **Semantyczny RAG** na sqlite-vec nad notatkami (single-user, dziś tylko źródło głosowe).
- On-device Whisper large-v3, własne pliki `.md` w Obsidian, per-folder szyfrowanie (CK/KEK + Touch ID), redaction firewall na egress do chmury.
- North star brain2: multi-source agregacja kontekstu (Slack/mail/kalendarz/Linear odłożone).

## Findings

| Produkt | (a) Local-first / on-device? | (b) Cross-meeting „pamięć" / ask-across-all | (c) MCP? | (d) Realtime AI na żywo | (e) Prywatność / szyfrowanie |
|---|---|---|---|---|---|
| **Granola** | Nie. Bot-free capture *lokalnie*, ale audio → Deepgram/AssemblyAI/OpenAI/Anthropic; notatki w US-AWS VPC. Audio kasowane po transkrypcji. Lokalna baza jest szyfrowana, ale przetwarzanie chmurowe. | **Mocne.** „Chat with your meetings", zapytania na poziomie folderów/kolekcji, „deal flow → market-intelligence DB". | **Tak** (hostowany, luty 2026; Claude/ChatGPT/Cursor). Basic: ostatnie 30 dni, bez transkryptów; Business/Enterprise: pełna historia. | Częściowe — robisz własne notatki w trakcie, AI dopisuje po; trochę live mid-call. Nie jest to mocny realtime-actions tool. | SOC2 Type 2 (lip 2025), brak treningu na danych, szyfr. at-rest/in-transit. Chmura. $1.5B wycena / $125M (mar 2026). |
| **Otter.ai** | Nie. Chmura, bot lub device capture. | **Bardzo mocne** — pozycjonowane jako „system of record"; agenci syntezują trendy z wielu calli, „wszystkie rozmowy z prospektem → next action". | **Tak — i serwer, i klient.** OAuth scoped. Najbardziej ambitne: Otter jako klient MCP wciąga Gmail/Drive/Notion/Salesforce/Jira/Slack do AI Chat (multi-source). | **Mocne** — Otter Meeting Agent: głosowo odpowiada w trakcie, planuje, pisze maile. | Chmura, OAuth per-user. Historycznie kontrowersje prywatności. Nie local-first. |
| **Fireflies.ai** | Nie. Chmura, bot-based. | Tak — AskFred, zapytania przez całą historię (feature requests, customer calls). | **Tak** (beta, używa istniejącego API key; transkrypty/metadata/speaker/summary; Claude/ChatGPT/Cursor/Devin). | Częściowe (AskFred w trakcie), bot. | Chmura, standard enterprise (SOC2 itd.). |
| **Fathom** | Nie. Chmura, opcja bot-free capture. | Tak — **„Ask Fathom"**, ChatGPT-style nad całą historią, cytowane odpowiedzi („co klient mówił o cenie w zeszłym kwartale"). | **Tak** (MCP server + turnkey Claude/ChatGPT, 2026). | Głównie post-meeting; bot-free capture. | Chmura, SOC2 Type II, GDPR, HIPAA, SSO/SCIM. |
| **tl;dv** | Nie. Chmura, **widoczny bot** dołącza do calla. | Tak — „Ask tl;dv" + multi-meeting reports, ale free = 10 czatów/mc; zaprojektowane raczej pod indywidualne użycie niż org-wide. | **Tak** (oficjalny serwer dla Zoom/Meet/Teams). | Bot-based nagrywanie; słaby realtime-actions. | Chmura, AES-256 at rest, SOC2 T2, GDPR, ISO 27001, EU AI Act aligned. |
| **Circleback** | Nie. Chmura, desktop bot-free + bot. | Tak (zarządzanie spotkaniami + ask), MCP + CLI. | **Tak** (MCP + CLI; Claude/ChatGPT/Cursor/Raycast — „wyciągnij nagranie z calla przez agenta"). | Głównie post. | Chmura, SOC2 Type II, EU-US DPF, HIPAA/BAA, szyfr. at-rest/in-transit, brak treningu. |
| **Limitless** (ex-Rewind) | **Było** strictly-local (Rewind), pivot do **„Confidential Cloud"** (TEE-style). Teraz chmura + pendant always-on. | **To jest cały produkt** — „digital memory", ask-everything nad życiem/spotkaniami, real-time transkrypcje. | **Tak** (podłącz „memory" do ChatGPT/Claude przez MCP URL). | Always-on capture + real-time transkrypcja. | Confidential Cloud + Consent Mode. **Przejęty przez Meta (ogł. 5 gru 2025); Rewind app wyłączony 19 gru 2025; sprzedaż pendanta kończona.** |
| **Cluely** | Nie. Chmura. Desktop overlay (macOS/Win): OCR ekranu + system audio → LLM → pływający overlay niewidoczny dla screen-share. | **Słabe / nie to** — realtime „ściągawka", nie długoterminowy kontekst. | Brak dowodu na serwer MCP. | **Najmocniejsze na rynku** — live odpowiedzi w trakcie rozmowy. | **Złe** — wyciek 2025 (83k+ userów), brak SOC2 T2, „undetectable"/covert = EU AI Act high-risk. CEO przyznał zawyżanie ARR (mar 2026). |
| **Spiral** (Every) | Nie. Chmura. | **Nie dotyczy** — to **AI writing partner / reusable-prompt** tool, nie recorder. Umie „rough notes → email w Twoim głosie", ale nie nagrywa/transkrybuje/buduje kontekstu spotkań. | Brak (nie meeting-MCP). | Nie. | Chmura; poza bezpośrednią kategorią. |

Confidence: Granola/Otter/Fireflies/Fathom/tl;dv/Circleback MCP + cross-meeting = **high** (potwierdzone na stronach producentów + KB). Limitless→Meta = **high** (TechCrunch/CNBC, gru 2025). Cluely brak MCP = **med** (brak dowodu = brak feature; nie udało się definitywnie wykluczyć). Spiral klasyfikacja = **high** (to narzędzie Every do pisania). Ceny/wyceny = punkt-w-czasie, datowane wyżej.

## Fit with Murmur's constraints
- **Local-first/prywatność:** tu Murmur jest praktycznie sam. **Żaden** z badanych nie jest end-to-end local-first dla transkrypcji *i* storage *i* posiadanych plików. Granola „local-capture" myli — audio i tak idzie do Deepgram/OpenAI; notatki w ich AWS. Limitless porzucił lokalność (→ Meta). Cluely to anty-wzór prywatności. To największy moat Murmura.
- **MCP seam:** lokalny loopback MCP Murmura jest **architektonicznie inny** niż reszta — oni robią OAuth do SaaS-backendu (dane już w chmurze), Murmur czyta lokalny SQLite, gate'owany przez `visibility_clause`. Pozycjonowanie: „MCP bez oddawania spotkań".
- **Obsidian-native / posiadane pliki:** żaden konkurent tego nie ma (wszyscy = walled-garden SaaS). Kolejny czysty wyróżnik.
- **brain2 multi-source:** tu konkurencja **napina** wizję Murmura — Otter (MCP-klient pull z Gmail/Salesforce/Notion/Slack/Jira) realizuje multi-source agregację *dziś* i szerzej. Murmur musi to dogonić jako lokalne, consent-gated tools (zgodnie z memory: connectors = live tools, nie wektoryzacja).

## Options & tradeoffs
1. **Pozycjonować na „local MCP + owned files", nie na „mamy MCP" (S).** Komunikat: jedyny meeting-second-brain, którego MCP/RAG działa bez wysyłania transkryptów do czyjejkolwiek chmury. Ryzyko: niskie. Unlock: ostra różnica vs cała 7-ka chmurowa.
2. **Dogonić zakres kontekstu — multi-source jako lokalne tools (M/L).** Otter wyprzedza brain2 w breadth. Pierwszy lokalny connector (web/kalendarz, consent-gated egress) zamyka narrację „drugi mózg, nie tylko notatnik". Ryzyko: scope creep, egress przez redaction firewall. Unlock: parytet wizji + utrzymanie moatu prywatności.
3. **Realtime live-actions (M, ostrożnie).** Cluely/Otter mają polished realtime; Murmur nie. Ale Cluely = etyczny/prywatnościowy anty-wzór. Wejście tylko jako *jawne, on-device* live-asysty (nie „undetectable"). Ryzyko: na realnym Macu + uprawnienia, honest bar wysoki.

## Recommendation & next step
Najpierw **Opcja 1** (czyste pozycjonowanie, zero kosztu inżynierskiego) + zacząć **Opcję 2** od jednego lokalnego źródła. Najmniejszy weryfikowalny plasterek: jednostronicowy battlecard „local MCP vs hosted MCP" (Murmur loopback/SQLCipher vs Granola/Otter OAuth-to-AWS) jako materiał do README/landingu — i spike pierwszego consent-gated connector-tool wystawionego przez istniejący lokalny MCP. De-risk: potwierdzić u 2-3 użytkowników, że „MCP bez chmury" + Obsidian-ownership to powód wyboru nad Granolą (która ma $1.5B i większy korpus).

## Open questions / what I couldn't verify
- Czy Cluely ma JAKIKOLWIEK serwer MCP — brak dowodu, ale nie wykluczyłem definitywnie (confidence med).
- Szczegóły „Confidential Cloud" Limitless po przejęciu Meta (czy memory/MCP przetrwa) — niepewne po 19 gru 2025.
- Nie re-greppowałem `mcp.rs`/RAG Murmura dla tego zadania — twierdzenia o naszym kodzie z kontekstu projektu, nie świeżej weryfikacji.
- Czy „cross-meeting" konkurentów realnie syntezuje (RAG) vs prosty keyword-recall — opisy marketingowe, nie testowane przeze mnie.

## Sources
1. https://www.granola.ai/blog/granola-mcp-claude-chatgpt-cursor — Granola MCP server (hostowany, plany, cross-meeting queries).
2. https://www.granola.ai/blog/chat-with-meetings-search-analyze-ai-2026 — Granola cross-meeting chat.
3. https://www.granola.ai/blog/local-first-ai-notetaker-vs-cloud + https://basilai.app/articles/2026-06-20-granola-vs-basil-bot-free-vs-on-device-privacy-architecture.html — Granola „local-capture" ale chmurowe przetwarzanie (Deepgram/OpenAI/AWS).
4. https://techcrunch.com/2026/03/25/granola-raises-125m-hits-1-5b-valuation-as-it-expands-from-meeting-notetaker-to-enterprise-ai-app/ — $125M / $1.5B (mar 2026).
5. https://otter.ai/blog/otter-mcp-your-meetings-now-power-every-tool-you-use + https://otter.ai/blog/otter-for-enterprise-connect-ai-to-ai-with-otters-mcp — Otter MCP serwer+klient, OAuth, multi-source.
6. https://otter.ai/blog/otter-meeting-agent-your-new-collaborative-teammate — Otter realtime voice agent.
7. https://www.fastcompany.com/91532774/otter-wants-its-ai-to-unlock-information-from-all-your-business-meetings — Otter „system of record"/cross-meeting.
8. https://fireflies.ai/blog/fireflies-mcp-server/ + https://docs.fireflies.ai/getting-started/mcp-configuration — Fireflies MCP (beta).
9. https://www.fathom.ai/overview + https://aiunpacker.com/blog/fathom-ai-note-taker-honest-review-after-90-days-of-use — Fathom MCP + „Ask Fathom".
10. https://tldv.io/blog/tldv-mcp-elevating-meeting-intelligence-with-ai-driven-contextualization/ — tl;dv MCP server (Zoom/Meet/Teams), ask/limity.
11. https://circleback.ai/releases + https://www.progressiverobot.com/2026/04/16/what-is-circleback-ai/ — Circleback MCP + CLI, prywatność.
12. https://techcrunch.com/2025/12/05/meta-acquires-ai-device-startup-limitless/ + https://www.cnbc.com/2025/12/05/meta-limitless-ai-wearable.html — Meta przejmuje Limitless; Rewind off 19 gru 2025.
13. https://www.limitless.ai/new + https://andrewschreiber.substack.com/p/an-early-adopters-thoughts-on-rewindais — Limitless Confidential Cloud (pivot z local), MCP URL.
14. https://tldv.io/blog/cluely-review/ + https://www.autoapplier.com/blog/cluely — Cluely realtime overlay, wyciek 83k, brak SOC2, EU AI Act.
15. https://every.to/on-every/introducing-spiral-v3-an-ai-writing-partner-with-taste — Spiral = AI writing partner (nie meeting tool).

## C2 — Second-brain / PKM engines

# Research: C2 — „Drugi mózg" / personal knowledge engines z MCP — czy ktoś buduje większy/bardziej oryginalny kontekst niż Murmur i ma MCP?

## Verdykt
Krótko: **nie istnieje narzędzie, które jednocześnie (a) buduje wyraźnie większy/bardziej oryginalny kontekst niż Murmur, (b) jest MCP-native i (c) jest local-first + owned-markdown.** Każdy mocny konkurent wygrywa jedną–dwie osie i przegrywa trzecią, którą Murmur trzyma. Najbliżej „bardziej potężnego ORAZ z MCP ORAZ lokalnego" jest **Pieces (LTM-2.7)** — realnie zasysa o rzędy wielkości więcej surowego kontekstu (wszystko co robisz, 9 miesięcy, wszystkie aplikacje) i wystawia to przez własny serwer MCP, w pełni on-device — ale płaci za to brakiem struktury/syntezy (pasywne „wysypisko", dokładnie ten failure mode, którego się obawiasz), zamkniętym kodem i brakiem Obsidiana. Najbliżej „bardziej oryginalnego architektonicznie + MCP-native + lokalnego" jest **Basic Memory** (markdown + SQLite + graf wiedzy przez rekurencyjny traversal, MCP-native) — to wręcz filozoficzny bliźniak north-star brain2, ale słabszy w skali i bez capture pipeline. **NotebookLM** ma największy surowy kontekst (1M tokenów / 25M słów) i najbardziej oryginalny output, ale jest zdyskwalifikowany: zero MCP, chmura-only Google, nie persystentny mózg.

## What we already have (grounded)
Murmur to już dziś, w kodzie, kompletny silnik kontekstu — nie „planowany":
- **Lokalny serwer MCP, read-only, zero egress** — `src-tauri/src/mcp.rs:1-3`, port `127.0.0.1:8765` (`mcp.rs:16`). To jest privacy-correct odwrotność chmurowego MCP Granoli/Otter.
- **6 narzędzi MCP** (`mcp.rs:230-260`, test `tools_list_has_six_tools`): `search_meetings`, `get_meeting`, `list_recent_meetings`, `search_semantic`, `get_open_commitments`, **`get_entity_dossier`** — to ostatnie składa „dossier" o osobie/projekcie (timeline wzmianek + otwarte zobowiązania + co-occurring encje, każde z cytatem `[[Title]]`).
- **Hybrydowy retrieval szyty source-agnostic**: seam embeddingowy BGE-M3 + tabela `vec0 float[N]` + **RRF fusion** FTS∪vector (`src-tauri/src/embed.rs:13-14, 25`) — **zmergowane, ale dormant w prod** za flagą `semantic_search_enabled` ze `StubEmbedder` (`RAG-BAKEOFF.md:4`). Real embedder = Phase 2.
- **GraphRAG-lite**: realne tabele `entities` + `entity_mentions` w SQLCipher (`db.rs:172/180`), graf czytany z gatingiem `visibility_clause`. Entity-anchored expansion to różnicownik, którego nie ma żaden meeting-tool (`PLAN-brain2-rag-voice.md:62`).
- **Jeden tool registry, trzy callery** (UI / MCP / głos) przez gated `execute_tool` (`src-tauri/src/tools.rs`, `orchestrate.rs`).
- **North star brain2** = source-agnostic ingest (`source_type` planowane), lokalny reasoning brain (mistral.rs, planowane Phase 3), Calendar/Slack/Jira jako leniwe tools (`PLAN-brain2-rag-voice.md`).
- Output = **owned `.md` w Obsidian vault** + per-folder lock/redaction firewall — czego praktycznie żaden konkurent poniżej nie łączy z MCP.

Czyli oś, na której Murmur jest dziś unikatowy: **diaryzowany głos jako first-class źródło + entity-graph dossiers + owned-markdown + lokalny serwer MCP + lock model**, wszystko nad jednym SQLite.

## Findings — tabela porównawcza (stan: czerwiec 2026)

| Narzędzie | (a) Local-first vs chmura | (b) Jak buduje DUŻY kontekst (skala) | (c) MCP-native? | (d) Bardziej oryginalne/potężne od Murmur? Czym / czemu nie |
|---|---|---|---|---|
| **Pieces (LTM-2.7)** | **Lokalnie** — „runs entirely on your device… nothing leaves your machine unless you share" [1][7] | **Ambient capture wszystkiego**: kod kopiowany, ekrany, audio (system+mic), **9-miesięczne okno** [9]; zapytania czasowe „co robiłem 3 mies. temu". Mechanizm storage/retrieval (vector/graf?) nieujawniony [7] | **TAK — własny serwer MCP** (`ask_pieces_ltm`) [1][2] | **POTĘŻNIEJSZY w SUROWYM wolumenie kontekstu** (cała aktywność, nie tylko spotkania) i lokalny + MCP. ALE: pasywne „wysypisko" bez syntezy/encji/owned-markdown, **closed-source**, ryzyko landfill (dokładnie to czego unikasz), brak lock/redaction. Conf: high |
| **NotebookLM (Google)** | **Chmura-only**, Gemini; dane tylko w context window sesji, nietrenowane [11][14] | **Największa surowa skala: do 25M słów / 1M-token okno** [11]; closed-RAG grounding + „Deep Research" agent (listopad 2025), Audio Overview/podcast [11] | **NIE** — brak MCP | Najbardziej oryginalny **output** (multimodal, podcast, deep research) i najbigger context, ALE **zdyskwalifikowany**: zero MCP, chmura, per-notebook efemeryczny (nie persystentny cross-corpus mózg), nie owned-files. Conf: high |
| **Khoj** | **Self-hostable** (AGPL-3.0) lub chmura; może użyć lokalnego LLM [3][4] | **RAG (pgvector)** nad web + PDF/Markdown/Word/Notion/org + repo; custom agenty, deep research, code sandbox [3] | **MCP klient, nie serwer** — integruje Jira/Linear/Slack *via* MCP; bycie serwerem MCP wciąż dyskutowane (#1022) [3][8] | Szerszy multi-source niż Murmur dziś, ALE retrieval = commodity pgvector (bez entity-graph), **konsumuje** MCP zamiast go wystawiać, brak lock/owned-md-jako-truth, AGPL. Conf: med-high |
| **Basic Memory** | **Local-first**, plik markdown na dysku „forever"; chmura opcjonalna [5][6] | **Graf wiedzy przez rekurencyjny traversal** (`build_context` idzie po relacjach, NIE similarity), encje+obserwacje+relacje z markdown; **SQLite jako indeks**; skala = rozmiar vaultu (mała/średnia) [6] | **TAK — natywnie MCP** (`write_note`/`read_note`/`search_notes`), AI i człowiek piszą do tych samych plików [5][6] | **Najbardziej oryginalny POKREWNY**: dokładnie wzorzec markdown+SQLite+graf+MCP, jaki goni brain2 — i **dwukierunkowy** (AI dopisuje do grafu). ALE: brak capture pipeline (zero głosu/audio), brak embeddingów domyślnie, mniejsza skala, brak lock/redaction. Conf: high |
| **Obsidian Copilot / Copilot Plus (Brevilabs)** | Free: tekst tylko do Twojego LLM; **Plus: konwersje plików na serwerach Brevilabs** [logancyang] | **Lokalny embedder → Vault Q&A (RAG)** nad całym vaultem; agentic w Plus (płatny) [obsidian] | **NIE natywnie** — to plugin Obsidiana; MCP dokładasz osobno (Obsidian MCP server) | Commodity vault-RAG; Murmur ma to + entity-graph + MCP + capture. Plus = częściowy egress (łamie local-first). Conf: med |
| **Smart Connections** | **Lokalnie**, on-device embeddings, zero API key [1-SC] | Lokalny vector index (`.smart-env/`), realtime reindex on edit; „related notes" similarity [1-SC] | **NIE** (plugin; MCP osobno) | Wąsko: tylko „related notes" semantic. Słabszy od Murmur (brak grafu encji, dossier, MCP). Conf: high |
| **Reor** | **W pełni lokalnie** (Ollama + Transformers.js + LanceDB) [reor] | Chunk+embed do **LanceDB**; **auto knowledge-graph przez vector similarity** (self-organizing, bez ręcznego linkowania); RAG Q&A [reor] | **NIE** (brak serwera MCP) | Najbliższy „lokalny auto-graf" ale: brak MCP, brak głosu/capture, brak lock, niewielka adopcja. Conf: med-high |
| **Mem (mem.ai)** | **Chmura** (OpenAI Startup Fund / a16z) [mem] | Self-organizing: AI linkuje notatki, Collections, Smart Search semantic, „Heads Up" panel [mem] | **NIE** (brak MCP serwera) | Chmura + proprietary lock-in = łamie oba moaty Murmur. Conf: med |
| **Cursor / Windsurf memory** | Lokalne reguły + chmurowy agent | „Memories" = auto-zapisane fakty/reguły z sesji (małe notki), nie korpus wiedzy [windsurf] | Pośrednio (agent ma własne tools) | Inny use-case (coding agent memory), trywialna skala. Nie jest knowledge engine. Conf: high |

## Fit with Murmur's constraints
- **Local-first**: tylko Pieces, Basic Memory, Smart Connections, Reor i (self-host) Khoj są naprawdę lokalne jak Murmur. NotebookLM/Mem i Copilot-Plus łamią ten moat. To natychmiast eliminuje większość „potężniejszych" jako wzorzec do skopiowania w całości.
- **Obsidian-native / owned-markdown**: **tylko Basic Memory i Murmur** traktują markdown jako prawdę + grają z Obsidianem. Pieces (potężniejszy w skali) nie ma owned-files — to dyskwalifikuje go jako „lepszego" dla Twojej tezy produktowej.
- **SQLite canonical**: Basic Memory używa dokładnie tego wzorca (markdown + SQLite-index) — walidacja, że architektura Murmur jest właściwa, nie ślepa uliczka.
- **MCP-native jako serwer** (nie klient): naprawdę spełniają to **Pieces, Basic Memory, Murmur** (+ zewnętrzne Obsidian-MCP serwery jak cyanheads/obsidian-mcp-server). Khoj jest klientem. To wąska liga — Murmur jest w niej.
- **Redaction firewall / lock**: **żaden** z konkurentów nie ma per-folder seal + redaction przed egressem. To czysty, niezduplikowany różnicownik.
- **Ostrzeżenie z Twojej własnej pamięci**: Pieces = żywy dowód „Mem/Rewind landfill risk" (`brain2-multisource-rag-roadmap.md:14`) — capture-everything bez retrieval/syntezy. To potwierdza, że „więcej surowego kontekstu" ≠ „lepszy mózg".

## Options & tradeoffs (co Murmur ma z tego zrobić)
- **A — Pożyczyć „ambient capture" od Pieces (S→L, wysokie ryzyko).** Odblokowuje największy wolumen kontekstu, ale wchodzi w landfill-trap, bloatuje DMG i napina local-first storage. Twoja roadmapa już to świadomie odrzuciła. Nie rekomenduję jako rdzeń; ewentualnie wąsko (np. tylko kalendarz/EventKit jako pierwsze nie-głosowe źródło, już zaplanowane).
- **B — Pożyczyć od Basic Memory „dwukierunkową, MCP-zapisywaną pamięć grafową" (M, niskie–śr. ryzyko).** Murmur dziś ma MCP read-only. Basic Memory pokazuje, że najwięcej oryginalności daje **AI dopisujące obserwacje/relacje z powrotem do grafu encji** (`build_context` graph traversal) + write-tools. To idealnie siada na istniejące `entities`/`entity_mentions` (`db.rs:172/180`) i seam tool-registry — i jest tańsze niż ambient capture. To jest najbliższy „bardziej oryginalny" wzorzec, który *pasuje* do Murmura.
- **C — Trzymać kurs (S, najniższe ryzyko).** Skończyć Phase 1 FTS5 + Phase 2 vector/GraphRAG-lite, wystawić `get_entity_dossier`/`semantic_search` przez MCP jako wedge. Differentiator = **integracja** (głos→graf→owned-md→lokalny MCP→lock), nie pojedyncza funkcja. Żaden konkurent nie łączy tej piątki.

## Recommendation & next step
**Trzymać kurs (C) + zaciągnąć jeden konkretny pomysł z Basic Memory (B).** Bottom line dla Twojej tezy: **nie ma narzędzia jednocześnie potężniejszego, bardziej oryginalnego I z MCP I lokalnego** — więc Murmur nie jest „w tyle"; jest w bardzo wąskiej (3-elementowej) lidze MCP-server-native + local. Jedyny realnie potężniejszy „huge context + MCP + local" to **Pieces**, ale wygrywa osią (pasywny wolumen), którą Twój produkt świadomie odrzucił jako landfill — to nie jest dług, to decyzja.

Najmniejszy weryfikowalny pierwszy slice de-riskujący przewagę: **spike „write-back to graph przez MCP"** — dodać 1 gated write-tool (np. `record_observation(entity, observation, relation)`) zasilający `entity_mentions`, na wzór Basic Memory `write_note`/`build_context`. To zamienia read-only MCP Murmura w dwukierunkowy mózg (jedyną oś, gdzie Basic Memory jest „bardziej oryginalny"), zostając w lock/visibility-gate. Dowód = test, że zapis przechodzi `visibility_clause` i że `get_entity_dossier` po zapisie zwraca nową relację z cytatem.

## Open questions / what I couldn't verify
- **Mechanizm storage/retrieval Pieces LTM** (vector? graf? rolling summaries?) — dokumentacja nie ujawnia [7]; nie potwierdziłem, czy 9-miesięczny kontekst jest „retrievalem", czy realnie wstrzykiwanym oknem. Conf: med.
- **Czy Khoj wystawia własny serwer MCP w czerwcu 2026** — dowody mówią „klient + dyskusja w toku" (#1022), ale to mogło się zmienić od czasu wątku; nie potwierdziłem na żywej wersji. Conf: med.
- **Realna jakość Polish recall** BGE-M3 vs te narzędzia — niezmierzone u nikogo; nasz własny RAG-BAKEOFF (`RAG-BAKEOFF.md`) jeszcze nieuruchomiony na żywym vaulcie (potrzebny real Mac).
- Nie testowałem żadnego z tych narzędzi na żywo — opieram się na docs/marketingu producentów (distrust-marketing flag dla skali NotebookLM 25M słów i 9 mies. Pieces — to liczby producenta).

## Sources
1. https://pieces.app/blog/introducing-the-pieces-mcp-server — Pieces MCP server (LTM jako tool dla zewn. AI)
2. https://docs.pieces.app/products/mcp — Pieces MCP docs (serwer, `ask_pieces_ltm`)
7. https://docs.pieces.app/products/core-dependencies/pieces-os/long-term-memory — LTM-2.7 „runs entirely on your device", capture kod/ekran/audio
9. https://pieces.app/features/long-term-memory/ai-memory-assistant + https://dev.to/nikl/introducing-pieces-mcp-server-...-9-months-context-window-4bp9 — 9-miesięczne okno
11. https://www.digitalocean.com/resources/articles/what-is-notebooklm + https://medium.com/@jimmisound/...notebooklm-evolution... — RAG grounding, 25M słów / 1M-token, Deep Research
3. https://github.com/khoj-ai/khoj — self-hostable RAG (pgvector), agenty, deep research
8. https://github.com/khoj-ai/khoj/discussions/1022 — status MCP w Khoj (klient/dyskusja)
5. https://www.basicmemory.com/ + https://github.com/basicmachines-co/basic-memory — MCP-native, markdown + lokalny
6. https://docs.basicmemory.com/start-here/what-is-basic-memory — graf przez rekurencyjny traversal (`build_context`), SQLite-index, NIE wektory
[1-SC] https://github.com/brianpetro/obsidian-smart-connections + https://smartconnections.app/ — lokalne on-device embeddings, `.smart-env/`
[logancyang]/[obsidian] https://github.com/logancyang/obsidian-copilot + https://community.obsidian.md/plugins/copilot — Vault Q&A RAG; Plus = częściowy egress (Brevilabs)
[reor] https://github.com/reorproject/reor — Ollama + LanceDB, auto knowledge-graph przez similarity, w pełni lokalny
[mem] https://mem.ai/ + https://techcrunch.com/2022/11/10/...mem-raises-23-5m-openai/ — chmura, self-organizing, OpenAI/a16z
[windsurf] https://www.arsturn.com/blog/understanding-windsurf-memories-system-persistent-context — auto-memories/rules (coding agent)
[obsidian-mcp] https://github.com/cyanheads/obsidian-mcp-server — zewn. serwer MCP do vaultu (14 tools, przez Local REST API)

Kluczowe ref-y kodu Murmura: `src-tauri/src/mcp.rs:1-3,16,230-260` (lokalny serwer MCP + 6 tools, w tym `get_entity_dossier`); `src-tauri/src/embed.rs:13-14,25` (BGE-M3 seam + vec0 + RRF, dormant); `src-tauri/src/storage/db.rs:172,180` (`entities`/`entity_mentions` graf); `src-tauri/src/tools.rs` + `orchestrate.rs` (gated tool registry, 3 callery); `docs/RAG-BAKEOFF.md:4`, `docs/PLAN-brain2-rag-voice.md:62` (GraphRAG-lite jako różnicownik); pamięć `brain2-multisource-rag-roadmap.md:14` (landfill-risk).

## C3 — Frontier memory/context engines

# Research: Frontier "context/memory engines" MCP-native — co Murmur może od nich przejąć (C3)

## Werdykt
Murmur ma już więcej niż "prosty RAG": shipuje hybrydę FTS5+`vec0` z RRF, encyjny GraphRAG-lite i dossier cross-meeting (`embed.rs`, `db.rs`, `summarize/dossier.rs`). Czego mu **realnie brakuje** względem frontu, to nie "więcej wektorów", tylko **warstwa FAKTÓW**: dziś trzyma tylko *nazwy* encji + log wzmianek (`entities`/`entity_mentions`, `db.rs:240`/`248`) — nie trzyma rozwiązanych, deduplikowanych, **wersjonowanych w czasie** faktów. Trzy rzeczy warte adaptacji, w tej kolejności: (1) **bitemporalna inwalidacja faktów** à la Graphiti/Zep — najbardziej oryginalna i idealnie zgodna z regułą "additive-only, never destroy"; (2) **ekstrakcja+rekoncyliacja faktów ADD/UPDATE/NOOP** à la mem0, podpięta pod lokalny mózg (Phase 3); (3) **observations/relations w markdownie** à la Basic Memory. Wszystko w czystym SQLite, bez zewnętrznego graph-DB i bez cloud-API. Reszta projektów (Letta agent-OS, Supermemory/Zep cloud, Cipher coding-specific, Memori/txtai) — walidują architekturę albo są nie-dla-nas.

## Co już mamy (grounded)
- **Hybryda FTS5 ∪ vektor + RRF** — `vec0` virtual table `vec_chunks`, `note_chunks`, RRF k=60 (`embed.rs:30`, `db.rs:360`/`951`); embedder za seam-em `Embedder`/`StubEmbedder`, realny BGE/e5 (candle) za feature `local-embed` (`embed.rs:20`, `embed/candle_bert.rs`). **Status: zmergowane, ale dormant w prod** (flaga `semantic_search_enabled` off + stub embedder) — patrz `docs/RAG-BAKEOFF.md`.
- **GraphRAG-lite (encyjny)** — `entities(name, kind)` + `entity_mentions(entity_id, meeting_id)` to **bipartytny indeks wzmianek**, nie graf relacji (`db.rs:240`/`248`). Sąsiedzi = współwystąpienie w tym samym spotkaniu (`entity_neighbors_visible`, dossier). Ekstrakcja = LLM zwraca **tylko listy nazw** people/projects z gotowej notatki, post-summary, chmurowo (`summarize/graph.rs:16`/`23`). Brak atrybutów, brak faktów, brak czasu ważności.
- **Dossier cross-meeting** — "stan [[Atlas]]/[[Anna]]": timeline wzmianek + open commitments + sąsiedzi, cytowane `[[Title]]`, gated (`summarize/dossier.rs`). To **timeline wzmianek**, nie *rozwiązany aktualny stan*.
- **Retrieval-augmented note-gen** — nowa notatka grounded w ~4 powiązanych wcześniejszych (`summarize/related_context.rs`), zawsze-on, gated.
- **MCP-native, read-only, zero egress** — tools: `search_meetings`, `get_meeting`, `search_semantic`, `get_open_commitments`, `get_entity_dossier` (`mcp.rs:230`). `source_type` kolumna już jest (`db.rs:350`) — source-agnostic groundwork. **Brak narzędzi WRITE.**
- **Roadmapa** już zakłada lokalny mózg (Phase 3, NER na surowym transkrypcie) + tool registry (`docs/PLAN-brain2-rag-voice.md`).

Innymi słowy: Murmur stoi mniej więcej tam, gdzie "vector + entity-linking" mem0 2026 — ale **bez warstwy faktów i bez czasu**.

## Findings — ranking silników (potęga × oryginalność × MCP-native × dopasowanie)

### Tier S — naprawdę budują "ogromny kontekst" potężniej niż prosty RAG

**1. Zep / Graphiti — temporalny knowledge graph (najbardziej oryginalne).** Każda krawędź ma jawny interwał ważności `(t_valid, t_invalid)` + osobno `created/expired` (model **bi-temporalny**: kiedy fakt obowiązywał vs kiedy go zapisano). Gdy nowy fakt **konfliktuje** ze starym, Graphiti go **inwaliduje (ustawia t_invalid), nie kasuje** — historia zachowana, "stan na teraz" i "stan miesiąc temu" oba odpowiadalne. Inkrementalny ingest "epizodów" (bez batch-recompute), retrieval hybrydowy (semantic + BM25 + graph traversal) **bez LLM-a w czasie retrievalu**, P95 ~300ms. **MCP-native** (eksperymentalny `mcp_server` w repo getzep/graphiti). [conf: high — potwierdzone w paper + neo4j blog]. Koszt: wymaga **Neo4j/FalkorDB** i **LLM-a przy ingeście**; Zep (managed) jest cloud. Zep raportuje przewagę nad MemGPT/full-context na DMR i LongMemEval (arxiv 2501.13956, sty 2025) [conf: med — benchmark autorów]. **To jest dokładnie ta zdolność, której Murmurowi brakuje.**

**2. Letta (MemGPT) — agent self-editing memory + sleep-time compute (oryginalny paradygmat "memory-as-OS").** Hierarchia: **Core memory** (bloki w oknie = RAM, które model sam edytuje tool-callami), **Recall** (historia), **Archival** (long-term przez query). **Sleep-time compute**: drugi agent dostaje "turę bez usera" i konsoliduje/przepisuje bloki pamięci w tle. Open-source, self-host, MCP-kompatybilny. [conf: high — letta.com/blog]. To nie "lepszy RAG" — to inny model (pamięć żyje w pętli agenta). Dla Murmura cenny jest **jeden** koncept: idle-time konsolidacja.

**3. mem0 / OpenMemory — ekstrakcja faktów + rekoncyliacja ADD/UPDATE/DELETE/NOOP.** Na `add()` robi **LLM-extraction** salient faktów, potem dla każdego: vector-similarity do istniejących → LLM decyduje **ADD/UPDATE/DELETE/NOOP** (dedup + spójność). 2026 algo: porzucili zewnętrzny graph-store na rzecz **wbudowanego entity-linkingu** + multi-signal fusion (semantic ∪ BM25 ∪ entity, znormalizowane) — **to jest dokładnie architektura, którą Murmur już ma na poziomie retrievalu**, ale mem0 dokłada warstwę reconcile faktów. Benchmarki (kwi 2026): LoCoMo 92.5 / ~7k tokenów-na-zapytanie vs ~26k full-context; LongMemEval 94.4; degradacja na skali (BEAM 1M→64.1, 10M→48.6) [conf: med — liczby producenta]. **OpenMemory MCP** = lokalny, privacy-first desktop memory manager wpinany do Claude/ChatGPT/Cursor [conf: high].

### Tier A — mocne, ale albo cloud, albo "walidacja architektury"

**4. Cognee — ECL (Extract → Cognify → Load) + ontologie.** Buduje **prawdziwy KG** (typowane encje+relacje) backed wektorem+graf-DB; **default w pełni lokalny: SQLite + LanceDB + Kuzu, embedded**, działa z Ollama; **first-party MCP server**. [conf: high — github topoteretes/cognee]. Najbliższy "dałoby się to złożyć lokalnie" — ale stack (Kuzu+LanceDB+Python) jest grubszy niż jeden SQLCipher.

**5. Basic Memory — architektonicznie najbliższy bliźniak Murmura.** **Markdown = źródło prawdy** + lokalny **SQLite index**, local-first ("plain text on your disk, forever"), **MCP stdio** (`write_note`/`read_note`/`build_context`, tools tagowane read-only/destructive/idempotent). Z markdownu buduje graf przez **Observations** (`- [category] fakt`) + **Relations** (`owns [[Target]]`), embeddingi FastEmbed. Python, **AGPL-3.0**, ~3.3k★ [conf: high — github basicmachines-co/basic-memory]. Nie jest "potężniejszym RAG-iem" — jest **dowodem, że wzorzec Murmura jest słuszny** i daje gotowy format observations/relations do podkradnięcia (AGPL = bierzemy wzorzec, nie kod).

### Tier B — oryginalne tezy, ale niszowe / nie-dla-nas

**6. Cipher (byterover) — dual System-1/System-2 memory.** System-1 = koncepty/fakty, **System-2 = zapis własnego rozumowania** agenta (uczy się ze swoich łańcuchów myśli) + Workspace memory dla zespołu. MCP-native, szeroka kompatybilność IDE. Oryginalne (pamięć rozumowania), ale **coding-agent-specific** — mało transferu do meeting-brain [conf: high — github/docs.byterover.dev].

**7. Supermemory — szybkie memory-API + "living knowledge graph".** Cały system to klient **centralnego API `api.supermemory.ai/v3`** (memory+RAG+connectors+profiles). Twierdzi "fully locally", ale rdzeń to cloud-API [conf: med — github supermemoryai/supermemory]. **Łamie local-first** — anty-wzorzec dla nas.

**8. Memori (GibsonAI) — SQL-native, anty-wektor.** Pamięć jako **zwykły SQL** (SQLite/PG/MySQL), "transparent + auditable", sesje grupują interakcje [conf: high — github GibsonAI/Memori]. Oryginalna teza ("nie potrzebujesz vector-DB"). Dla nas to **walidacja "SQLite is canonical"** — ale Memori jest *prostszy* niż Murmur, nie potężniejszy.

**9. txtai — embeddings DB + semantic graph (lokalnie, SQLite).** "Unia vector index + graph + relational"; semantic graph gdzie węzły łączy podobieństwo. Toolkit, nie silnik pamięci; mało nowego ponad to, co Murmur ma [conf: high — neuml/txtai].

**10. LlamaIndex / LangChain memory + MCP — commodity glue.** Frameworki/adaptery, nie "engine"; nic do przejęcia poza wzorcami API. **Skip.**

## Fit z ograniczeniami Murmura
- **Local-first / privacy:** Graphiti-temporal i mem0-reconcile da się zrobić **w 100% lokalnie** (czysty SQLite + lokalny mózg z Phase 3). Cloud-warianty (Zep, mem0 Platform, Supermemory, Cognee+chmurowy LLM) **odpadają** — egress. Każda ekstrakcja faktów z surowego transkryptu **musi iść przez lokalny mózg** (zero-egress), nie przez `claude_code`.
- **Obsidian-native:** wzorzec Basic Memory (observations/relations w `.md`) jest **wprost zgodny** — fakty stają się czytelnymi liniami na stronie `[[Person]]/[[Project]]`, zero lock-in.
- **SQLite-canonical:** bitemporalne fakty to **nowa tabela w istniejącym SQLCipher** (`facts(subject, predicate, object, valid_at, invalid_at, recorded_at, source_meeting_id)`), nie trzeci store. **NIE wnosić Neo4j/Kuzu/FalkorDB/LanceDB** — łamie bundled + SQLCipher-at-rest.
- **Lock model:** to jest najcięższe ograniczenie. Fakty są **derywatem treści spotkania** → muszą być gated (`visibility_clause`) i **purge-on-lock** jak `vec_chunks` (`db.rs:873`). Fakt wyekstrahowany z zalockowanego folderu **nie może wyciec** do dossier ani MCP. Inwalidacja = `UPDATE invalid_at`, czyli **additive, nigdy DELETE** — idealnie pasuje do reguły "no destructive migration".
- **Provider seam + redakcja:** synteza "stanu encji" nadal jedzie przez `SummarizerProvider` za firewallem redakcji; ekstrakcja faktów to nowy seam lokalnego mózgu (osobny od cloud-summarizera, jak planuje Phase 3).
- **CI / honesty:** ekstrakcja+reconcile+bitemporal są **w pełni headless-testowalne** (`cargo test --lib`, deterministyczne reguły invalidacji). Jakość ekstrakcji na **polskim ASR** — niemierzalna headless, potrzebny realny Mac (jak RAG-bakeoff).

## Opcje i tradeoffy

**Opcja A — Bitemporalna tabela faktów + reconcile (Graphiti×mem0, lokalnie). [M]**
Nowa `facts` (SQLCipher, gated, purge-on-lock); lokalny mózg ekstrahuje `(subject, predicate, object)` z transkryptu; reguła ADD/UPDATE/**INVALIDATE(t_invalid)**/NOOP względem istniejących. Dossier czyta **aktualnie-ważne** fakty + opcjonalnie historię.
- *Unlocks:* "jaki jest **teraz** stan Atlasa" + "co się **zmieniło** od miesiąca" — czego dziś nie da się odpowiedzieć. Najbardziej oryginalny ruch, w pełni local-first.
- *Risk:* jakość ekstrakcji PL (mierzalne dopiero na realnym Macu); zależność od Phase-3 mózgu; lock-security review obowiązkowy (nowy read+derive surface).

**Opcja B — Observations/Relations do markdownu (Basic Memory). [S]**
Rozszerz `summarize/graph.rs` z "tylko nazwy" o **typowane observations** (`- [decision] deadline → piątek (2026-06-20)`) i **relations** (`owns [[Atlas]]`) zapisywane na stronach encji w vaulcie.
- *Unlocks:* czytelny dla człowieka, traversowalny przez MCP/Obsidian graf — bez nowego store'u. Najtańsze, czysto Obsidian-native.
- *Risk:* bez warstwy faktów z Opcji A to wciąż append (duplikaty/staleness); rozwiązuje czytelność, nie aktualność.

**Opcja C — Sleep-time konsolidacja (Letta). [M, dopiero po Phase 3]**
Idle-time, on-device pass: dedup faktów, inwalidacja stale, przepisanie digest/dossier — zero egress.
- *Unlocks:* "drugi mózg mądrzeje gdy nie patrzysz". Mocny hook produktowy.
- *Risk:* tylko z lokalnym mózgiem; ryzyko cichej korupcji pamięci → musi być additive + odwracalne.

**(Odrzucić: zewnętrzny graph-DB, cloud memory-API, pełny rewrite na agent-OS Letta — wszystkie łamią twarde ograniczenia.)**

## Rekomendacja i następny krok
Zrób **Opcję A jako rdzeń, z Opcją B jako warstwą prezentacji** — to łączy najbardziej oryginalny pomysł frontu (bitemporalna inwalidacja Graphiti) z najlepiej sprawdzonym wzorcem ekstrakcji (mem0 reconcile), w całości w SQLCipher, bez nowych zależności.

**Najmniejszy weryfikowalny pierwszy plasterek (de-risk, headless, zero nowego mózgu):**
1. Dodaj **additywną** tabelę `facts` z `valid_at/invalid_at/recorded_at/source_meeting_id` + `visibility_clause` + purge-on-lock (mirror `vec_chunks`).
2. Napisz **czystą, deterministyczną funkcję reconcile** (ADD/UPDATE/INVALIDATE/NOOP) z RED-before-GREEN testami konfliktu ("deadline=czwartek" potem "deadline=piątek" → stary dostaje `invalid_at`, oba w historii, dossier pokazuje piątek). To dowodzi **mechaniki bitemporalnej bez LLM-a** — najryzykowniejsza część jest wtedy zweryfikowana zanim wejdzie ekstrakcja.
3. Dopiero potem podłącz ekstraktor pod lokalny mózg (Phase 3) i zmierz jakość na realnym polskim vaulcie (rozszerz `docs/RAG-BAKEOFF.md` o oś "fact-correctness/temporal").

To jeden spike (~SQLite + reguły, w pełni `cargo test --lib`) + obowiązkowy `lock-security-reviewer`.

## Open questions / czego nie zweryfikowałem
- **Jakość ekstrakcji faktów na polskim, code-switched ASR** — niemierzalne headless; ten sam unknown co BGE-M3 recall w bake-offie. Potrzebny realny Mac.
- **Benchmarki mem0/Zep** (LoCoMo/LongMemEval/DMR) to liczby producentów — traktować jako kierunkowe, nie jako prawdę [point-in-time: mem0 kwi 2026, Zep paper sty 2025].
- **Eksperymentalny status MCP-serverów** Graphiti/Cognee — nie weryfikowałem stabilności/wersji kodu, tylko README/blog.
- **Letta sleep-time / Cipher System-2** opisane z bloga producenta — nie czytałem kodu źródłowego.
- Nie sprawdziłem, czy `entity_mentions` ma gdzieś już ukryty timestamp ważności poza `created_at` (z lektury — nie ma; to czysty mention-log).

## Sources
1. https://mem0.ai/blog/state-of-ai-agent-memory-2026 — benchmarki + 2026 algo (entity-linking + multi-signal fusion), kwi 2026.
2. https://arxiv.org/pdf/2504.19413 — Mem0 paper: faza Extraction + Update (ADD/UPDATE/DELETE/NOOP), Mem0g graph variant.
3. https://deepwiki.com/mem0ai/mem0/3.3-history-and-storage-management — operacje pamięci mem0.
4. https://arxiv.org/abs/2501.13956 — Zep: A Temporal Knowledge Graph Architecture for Agent Memory (bi-temporal, DMR/LongMemEval), sty 2025.
5. https://neo4j.com/blog/developer/graphiti-knowledge-graph-memory/ — Graphiti: bitemporal `t_valid/t_invalid`, edge invalidation, hybrid retrieval, P95 300ms, Neo4j backend.
6. https://github.com/getzep/graphiti/blob/main/mcp_server/README.md — eksperymentalny MCP server Graphiti.
7. https://www.letta.com/blog/memory-blocks/ + https://www.letta.com/blog/sleep-time-compute/ — core/recall/archival + sleep-time compute.
8. https://github.com/topoteretes/cognee — ECL pipeline; lokalny default SQLite+LanceDB+Kuzu; first-party MCP.
9. https://github.com/basicmachines-co/basic-memory — markdown=truth + SQLite index + MCP + observations/relations (AGPL-3.0).
10. https://github.com/byterover/cipher (via docs.byterover.dev/cipher/overview) — dual System-1/System-2 + workspace memory.
11. https://github.com/supermemoryai/supermemory — memory API + living KG (cloud-API-centric).
12. https://github.com/GibsonAI/Memori + https://www.marktechpost.com/2025/09/08/gibsonai-releases-memori... — SQL-native memory engine, wrz 2025.
13. https://neuml.github.io/txtai/ — embeddings DB = vector ∪ graph ∪ relational, SQLite local.
- file:line (nasze): `src-tauri/src/embed.rs:20`/`30`, `src-tauri/src/storage/db.rs:240`/`248`/`350`/`360`/`873`/`951`, `src-tauri/src/summarize/graph.rs:16`/`23`, `src-tauri/src/summarize/dossier.rs`, `src-tauri/src/summarize/related_context.rs`, `src-tauri/src/mcp.rs:230`, `docs/RAG-BAKEOFF.md`, `docs/PLAN-brain2-rag-voice.md`.
