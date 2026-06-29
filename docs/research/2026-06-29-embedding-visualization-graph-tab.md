<!-- Generated 2026-06-29 via /research (murmur-researcher fan-out, 4 angles). Pricing/funding/version = point-in-time. -->
# Research: UI do podglądu i analizy wektorów (embeddingów) w zakładce Graf

> **KOREKTA (2026-06-29, po sprawdzeniu kodu vs zaktualizowane CLAUDE.md/pamięć):** dwa twierdzenia agentów są NIEAKTUALNE — kod jest dalej niż zakładali:
> 1. **Embedder NIE jest już za feature-gatem `--features local-embed`.** `Cargo.toml` ma `default = []` (`Cargo.toml:147`); candle/mistralrs/embedder są **zawsze kompilowane** i prawdziwy `CandleBertEmbedder` aktywuje się w runtime gdy `embed_model_present()` (`embed.rs:146-160`), inaczej `StubEmbedder`. Czyli "wymaga specjalnego buildu" → **fałsz**; wymaga tylko **pobranego modelu** e5. ("StubEmbedder na domyślnym buildzie" wciąż prawdą *dopóki user nie pobierze modelu*.)
> 2. **Gated semantic search już istnieje i jest używany.** `search_semantic_visible(&qvec, k, unlocked)` (`db.rs:937`) i `search_hybrid_visible` (FTS∪wektor∪graf-encji RRF, `db.rs:1007`) działają w `tools.rs:103`, `pipeline.rs:1077`, `commands.rs:5456`. Opcja A (`related_meetings`) jest więc jeszcze cieńsza niż opisano.
>
> Reszta werdyktu (atlas = gadżet/timing; flaga `semantic_search_enabled` domyślnie OFF, `config.rs:223`; bake-off nieodpalony) **trzyma się bez zmian**.

## TL;DR / Verdict

**Nie buduj end-userowego "atlasu embeddingów" / scatter-plota 2D w zakładce Graf — to dziś gadżet, nie differentiator, i w domyślnym buildzie wizualizowałby śmieci.** Trzy niezależne powody, które wyszły zgodnie u wszystkich agentów:

1. **Warstwa wektorowa jest uśpiona.** Domyślnie `semantic_search_enabled = false` (`settings/config.rs:223`), a embedder to deterministyczny `StubEmbedder` (hash-bag, semantycznie bez znaczenia) — prawdziwy e5 kompiluje się tylko pod `--features local-embed` + pobrany model (`embed.rs:113-162`). Mapa na domyślnym buildzie pokazuje strukturę worka słów, nie znaczenie.
2. **Wartość wektorów jest jeszcze nieudowodniona.** `docs/RAG-BAKEOFF.md` to wciąż nieodpalony protokół, który ma rozstrzygnąć, czy wektory w ogóle biją już-shippnięte FTS5 przy naszym małym korpusie. Nie wiadomo, czy jest co wizualizować.
3. **2D projekcja na małym korpusie + dla usera notatek to słaby produkt.** UMAP/t-SNE potrzebują tysięcy punktów, żeby coś znaczyły; nasz korpus to ~10²–10³ chunków → blob/hairball. Nawet zwykły graf Obsidiana jest powszechnie oceniany jako "ładny, ale bezużyteczny" przy małych vaultach.

**Co MA sens zamiast tego:** realny user-value to nie "mapa wektorów", tylko afordancja **"Powiązane wg znaczenia"** (semantyczni sąsiedzi spotkania/encji) — i jej dom to detal encji / Ask, a nie nowy widok w Grafie. Plus, osobno, **diagnostyczny** podgląd embeddingów dla nas (walidacja jakości w trakcie bake-offu), który należy do dev-toola/Settings, nie do shippowanego Grafu.

**Confidence: high.** Techniczna wykonalność scatter-plota jest wysoka (PCA w Rust, bez nowych zależności) — problem jest produktowy (wartość/timing), nie inżynieryjny.

---

## Co już mamy (z repo, file:line)

- **Dwa rozłączne grafy, celowo osobne.** Zakładka Graf = katalog encji (People/Projects) z krawędziami **co-occurrence** (wspólne widoczne spotkania), backed przez `entities`+`entity_mentions`, gated `visibility_clause`/`list_entities_visible` (`db.rs:1448`). Renderuje karty + jednoencyjny neighborhood-SVG — **nie** wektory (`graph.component.ts:25-38`).
- **Warstwa wektorowa istnieje, ale uśpiona.** `note_chunks` + `vec_chunks` (sqlite-vec `vec0 float[384]`, `db.rs:343-368`), `EMBED_DIM=384`, model multilingual-e5-small (`embed.rs:28,81-90`), KNN + RRF fuzja FTS∪wektory (`search_semantic_visible` `db.rs:937-995`, `rrf_fuse` `embed.rs:245`). Ale: domyślnie OFF (`config.rs:223`), embedder = stub bez `--features local-embed` (`embed.rs:113-162`), reindex ręczny (`reindexEmbeddings`, `ipc.service.ts:456-486`).
- **Lock-model już ogarnia wektory.** Chunki/wektory indeksowane **tylko dla widocznych** spotkań (`pipeline.rs:696-709`, `db.rs:802-871`); **purge-on-lock w tej samej transakcji co seal** (`purge_chunks_tx`, `db.rs:876-912`; spięte w `lock_folder` `commands.rs:3091-3098`, move-into-locked, startup relock reconcile `db.rs:1893-1909`, `delete_meeting`); ready **defense-in-depth** re-aplikują `visibility_clause` (`db.rs:947/963`). Kod **jawnie** komentuje, że embedding jest odwracalny do treści: *"a dense embedding is invertible … must NOT survive at rest"* (`commands.rs:2946-2948`, `db.rs:339-342`). Testy: `vec_semantic_search_is_gated_by_visibility` (`db.rs:4173`), `vec_chunks_purged_on_lock`.
- **Brak jakiejkolwiek wizualizacji wektorów / projekcji.** Grep nie znajduje PCA/UMAP/t-SNE ani w Rust, ani w `src/app/`. Renderer grafu to **nie** silnik: `graph.component.ts` to katalog kart; jedyne rysowanie węzłów to czysty SVG w `entity-neighborhood.component.ts:330-372` (pozycje z `cos/sin` w `computed()`, zero symulacji/rAF). FE deps: tylko `@angular/*`, `@tauri-apps/api`, `rxjs`.

---

## Findings (per kąt)

### A. Sens produktowy i konkurencja
- **Nawet lider kategorii nie sprzedaje surowego atlasu embeddingów jako wartości.** Obsidian Smart Connections opakowuje semantykę jako *discovery + context assembly*; ich "Visualizer" to force-graph *sąsiadów bieżącej notatki* (neighborhood), a "Smart Graph" (klastry) jest **Pro + experimental** i służy do wybierania źródeł do kontekstu AI (`github.com/Mossy1022/Smart-Connections-Visualizer`, `smartconnections.app/smart-graph/`). Confidence: high.
- **Globalna projekcja 2D to instrument data-science/debug, nie prawda dla end-usera.** Konsensus: 2D zniekształca odległości w high-dim, daje "arbitrary shapes" — użyteczne do QC/debug z zastrzeżeniami (arxiv 2505.06386; PMC11446450). Narzędzia (TensorBoard Projector, Nomic Atlas) celują w praktyków ML. Confidence: high.
- **Sygnał popytu skręca w stronę search/Ask, nie map.** Nawet zwykły graf Obsidiana: "imponujący, ale nie najszybsza droga do informacji; search/tagi wygrywają", mało wnosi przy małych vaultach (aiproductivity.ai, lindy.ai). Confidence: med-high.

### B. Feasibility techniczna
- **Redukcja wymiarów: w Rust (backend), nie w JS.** Surowe wektory to gated bloby w SQLCipher; wysyłanie N×384 floatów do webview marnotrawne i poszerza powierzchnię leak. PCA wymaga centrowania/rzutowania wszystkich wektorów razem → naturalna komenda batch.
- **PCA-2D bez żadnej nowej biblioteki.** Top-2 składowe = **power iteration + deflacja** na macierzy kowariancji 384×384 (~tylko mnożenia macierz-wektor), ~kilkadziesiąt iteracji, milisekundy dla 10⁴ chunków. Zero nowych crate'ów, działa w **domyślnym** buildzie (candle jest feature-gated, więc nie wiązać projekcji z candle). Confidence: high.
- **PCA vs t-SNE vs UMAP:** PCA = deterministyczny, zero-dep, pure Rust → **MVP**. t-SNE = lepsze klastry, ale O(N²), niedeterministyczny, nowy crate → defer. UMAP = najlepszy, ale ciężki (nowy crate) → skip. Confidence: high.
- **Skala → SVG wystarczy, bez WebGL.** Chunki ~800 znaków (`embed.rs:31`), spotkanie → ~2–10 chunków, setki spotkań → ~10²–10³ punktów. SVG spokojnie renderuje ~1–2K `<circle>`. Canvas dopiero przy kilku tysiącach. Confidence: med (liczba per-user szacowana, niemierzona).
- **Jedyna niezweryfikowana mechanika:** czy `vec0` pozwala odczytać zapisany wektor zwykłym `SELECT embedding` (znamy tylko ścieżkę `MATCH`/KNN). Do potwierdzenia spike'em. Confidence: med.

### C. Fit z lock-modelem (najważniejsze dla bezpieczeństwa)
- **Surowy embedding 384D JEST leakiem treści.** vec2text odtwarza tekst z gęstych embeddingów: ~92% exact dla krótkich tekstów, odzyskuje nazwiska z notatek klinicznych (arxiv 2310.06816). Modele **wielojęzyczne (e5!) są bardziej podatne** (ACL 2024, 2024.acl-long.422). Atakujący **nie potrzebuje naszego modelu** — przestrzenie embeddingów są ~izomorficzne, wystarczy jedna para (ALGEN, arxiv 2502.11308; zero-shot 2504.00147). → **wektor nigdy nie może opuścić Rusta**; projekcja 2D (lossy, nieodwracalna) jest OK. Confidence: high.
- **Inwarianty, które nowa komenda MUSI spełnić, by przejść lock-security-review:**
  1. Źródło danych przez **ten sam** `visibility_clause`/`unlocked`-set `EXISTS`, co `search_semantic_visible` (`db.rs:947/963`) — żadnej nowej ungated ścieżki read.
  2. **Surowy embedding nigdy nie przekracza IPC** — projekcja 384→2D w Rust; DTO = tylko `{x, y, meeting_id, label}`.
  3. **Labele = treść → masked dla niewidocznych** (tytuł/snippet → "🔒 Locked", wzór `commands.rs:1431/1467`).
  4. **Brak nowego at-rest store** wektorów/projekcji zablokowanej treści; jeśli cache, to tylko widoczne punkty, kluczowane `content_hash`, purgowane razem z chunkami przy lock.
  5. **RED-przed-GREEN** regresja widoczności (wzór `db.rs:4173`).
  6. Brak PII w logach; brak nowych npm/FFI.
- **Dobra wiadomość:** ta funkcja jest **w pełni weryfikowalna headless** (deterministyczne PCA + gated SQL) — rzadkość dla zmiany dotykającej lock-modelu, brak "needs a real Mac" dla samego gating/projekcji.

### D. UX wpięcia
- **Prior art: semantyczna mapa i graf linków to celowo DWA różne widoki** ("meaning-based neighborhoods" vs "explicit link structure", `smartconnections.app/smart-graph/`). Wciskanie wektorów do tej samej "mapy" co encje myli usera — odpowiadają na inne pytania.
- **Z trzech use-case'ów wart UI jest tylko jeden:**
  - *"Znajdź podobne spotkania/fragmenty"* → **najwartościowszy**, ale to lista "Powiązane wg znaczenia", nie mapa. Backend już liczy (`search_semantic_visible`).
  - *"Klastry tematów"* → ładne demo, niska akcja; bezwartościowe na stubie.
  - *"Czemu Ask zwrócił to a nie tamto"* → realna explainability, ale dom = Ask (retrieved chunks + score), nie Graf.
- **Zgodność zoneless/signals gotowa do skopiowania:** `effect()`+`await ipc.x()`→signal ze stale-guard (`entity-detail.component.ts:305-336`), OnPush, `@if/@for track id`, inline template, `var(--token)`, opaque overlay (trap T3). Toggle trybu = `signal<'entities'|'topics'>`.

---

## Fit z ograniczeniami Murmur

- **Local-first:** ✅ wszystko on-device, zero egress (uwaga: nie pipe'ować labeli do LLM → wtedy redaction firewall N/D).
- **Obsidian-native / SQLite-canonical:** ✅ cienki reader nad `vec_chunks`/`note_chunks`, brak drugiej kopii prawdy.
- **Lock-model:** ⚠️ load-bearing — OK tylko przy spełnieniu inwariantów C; to zmiana **gated przez lock-security-reviewer**.
- **macOS/CI:** ✅ gating+projekcja w pełni headless. ⚠️ **ale** sensowność klastrów na prawdziwych (polskich) notatkach = tylko `--features local-embed` + e5 + reindex na realnym Macu. Domyślny CI build = stub → ryzyko fałszywego GREEN (zielony test nad bezsensownym stubem).
- **No new deps:** ✅ PCA w pure Rust + inline SVG; UMAP/t-SNE/d3 odpadają z reguły.

---

## Opcje i tradeoffy

| Opcja | Co to | Effort | Ryzyko | Werdykt |
|---|---|---|---|---|
| **A. "Powiązane wg znaczenia"** w detalu encji/spotkania (lista/chipy sąsiadów z `search_semantic_visible`) | realny discovery, zero nowego widoku, reużywa gated query, działa nawet na stubie (degraduje wdzięcznie) | **S** | niskie | **Rób — to właściwy "vector view"** |
| **B. Atlas/scatter 2D w Grafie** (toggle "Encje\|Tematy", PCA w Rust → SVG) | "wow demo"; technicznie wykonalne tanio | **M→L** | wysokie: bezwartościowe na stubie, nowa szeroka ścieżka read (leak risk), koszt utrzymania, blob przy małym korpusie | **Defer** do zbundlowanego+zwalidowanego e5 |
| **C. Diagnostyka embeddingów** w Settings/Ask (status modelu, #chunków, "test nearest-neighbor", "Źródła+score" pod odpowiedzią Ask) | explainability/zaufanie/debug | **S** | niskie | **Rozważ osobno** — dom dla use-case "czemu Ask zwrócił to" |
| **D. Internal dev/notebook dump** `vec_chunks` do walidacji jakości w bake-offie | odpowiada na pytanie "czy e5 > FTS" | **S** | ~zero | **Zrób przy bake-offie** (taniej poza Angularem) |

---

## Rekomendacja i pierwszy krok

**Kolejność:** najpierw odpal bake-off (D) → potem A → B/C tylko jeśli A i bake-off to uzasadnią. Atlas (B) odrzuć teraz.

- **Krok 0 (de-risk całości):** odpal `docs/RAG-BAKEOFF.md` z realnym `--features local-embed` buildem na prawdziwym vaulcie, i przy okazji zrzuć embeddingi do notebooka, żeby zobaczyć czy klastry mają sens (czy e5 separuje PL/EN, czy chunki są zdrowe). Jeśli e5 nie bije FTS5 przy naszym korpusie — całe pytanie o wizualizację jest moot.
- **Najmniejszy weryfikowalny user-facing plaster (Opcja A):** komenda `related_meetings(meeting_id, k)` = cienki wrapper na `search_semantic_visible` (gated, dedup po spotkaniu) + RED-przed-GREEN test widoczności (wzór `db.rs:4173`) + jedna metoda w `ipc.service.ts` + sekcja "Powiązane wg znaczenia" (chipy `app-sources`) w `entity-detail`. Zero projekcji, zero nowego widoku, zgodne z lock-modelem.
- **Jeśli kiedyś B:** backend-only spike — `project_chunks_2d(visibility)` (power-iteration PCA, sign-canonicalized dla stabilności znaku) z `cargo test --lib` na one-hot/near-aligned wektorach (helpery `db.rs:4107/4127`): assert (a) separacja różnych kierunków, (b) determinizm. To domyka i matematykę PCA, i odczyt wektora z `vec0` headless, zanim ruszy FE.

---

## Otwarte pytania / czego nie udało się zweryfikować

- **Czy e5 realnie bije FTS5 na tym vaulcie?** — niezweryfikowane, wymaga bake-offu na Macu (stub jest bez znaczenia).
- **Czy `vec0` zwraca zapisany wektor zwykłym `SELECT embedding`?** — znamy tylko ścieżkę `MATCH`/KNN; do potwierdzenia spike'em (med confidence że działa).
- **Realna liczba chunków per user** — szacowana z rozmiaru chunka, niemierzona; jeśli power-userzy przekraczają kilka tysięcy punktów → rozważyć canvas (Opcja B).
- **Czy prawdziwy embedder jest wpinany w jakimkolwiek shipowanym buildzie** (release flags `local-embed`) — niezweryfikowane; domyślny `cargo test --lib` = stub.
- **Polish-specific inversion rate** — literatura potwierdza, że multilingual e5-class są odwracalne (często bardziej niż EN); brak liczby dla polskiego, ale konserwatywne założenie (wektor = pełne PII) trzyma się niezależnie.
- **Smart Graph adoption/retention** — brak publicznych liczb; opieramy się na framingu vendora + opiniach.

---

## Sources

**Web:**
1. https://github.com/Mossy1022/Smart-Connections-Visualizer — force-graph sąsiadów notatki (neighborhood, nie atlas).
2. https://smartconnections.app/smart-graph/ — semantyczna mapa trzymana osobno od grafu linków; Pro/experimental, cel = wybór źródeł do kontekstu AI.
3. https://github.com/brianpetro/obsidian-smart-connections — wiodący local-first on-device-embeddings plugin (pozycjonowanie: related notes + search, nie viz).
4. https://arxiv.org/pdf/2505.06386 — Apple "Embedding Atlas": viz embeddingów to narzędzie praktyka.
5. https://www.ncbi.nlm.nih.gov/pmc/articles/PMC11446450/ — 2D embeddingi zniekształcają odległości; QC/hipotezy z zastrzeżeniami.
6. https://aiproductivity.ai/blog/best-note-taking-apps-graph-views/ — graf "imponujący, ale nie najszybszy do informacji".
7. https://www.lindy.ai/blog/obsidian-review — graf mało wnosi przy małych vaultach.
8. https://arxiv.org/abs/2310.06816 — vec2text: inwersja embeddingów do tekstu (~92% krótkich, odzyskuje nazwiska).
9. https://aclanthology.org/2024.acl-long.422.pdf — embeddingi wielojęzyczne często bardziej podatne na inwersję.
10. https://arxiv.org/abs/2502.11308 — ALGEN: few/one-shot linear-alignment inversion (atakujący nie potrzebuje twojego modelu).
11. https://arxiv.org/abs/2504.00147 — Universal Zero-shot Embedding Inversion.
12. https://en.wikipedia.org/wiki/Power_iteration — top-k eigenvectors bez biblioteki SVD (PCA).
13. https://alexgarcia.xyz/sqlite-vec/ — sqlite-vec vec0 (mechanika read-back do potwierdzenia).

**Kod:**
- `src-tauri/src/embed.rs:28,81-90,113-162,200-206,215-238,245,31` — EMBED_DIM, model e5, StubEmbedder/gating, blob packing, RRF, chunk z nagłówkiem title·date, rozmiar chunka.
- `src-tauri/src/storage/db.rs:343-368,802-871,876-912,933-995,1007,1269,1448,1893-1909,4107,4127,4173` — schema vec, indeks visible-only, purge-on-lock, gated KNN, RRF, `visibility_clause`, entity graph, relock reconcile, test helpery, test gatingu.
- `src-tauri/src/commands.rs:2946-2950,3091-3098,1431/1467` — purge-on-seal + rationale "embedding invertible", masked DTO.
- `src-tauri/src/pipeline.rs:696-709` — gated indexing. `src-tauri/src/settings/config.rs:223` — `semantic_search_enabled=false` default.
- `src/app/features/graph/graph.component.ts:25-38,166-194,457-521`, `entity-neighborhood.component.ts:330-372`, `entity-detail.component.ts:305-336`, `entity-card.component.ts` — graf = katalog + neighborhood-SVG, blessed effect-pattern.
- `src/app/core/models.ts:100-107,501-567`, `src/app/core/ipc.service.ts:266-283,456-486` — typy grafu/RAG, komendy embeddingów (brak metody wystawiającej wektory).
- `docs/RAG-BAKEOFF.md` — wartość wektorów wciąż nieudowodniona vs FTS5.
